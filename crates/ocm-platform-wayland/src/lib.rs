use ocm_core::{Config, KeyboardInteractivity, Layer, PlatformError};
use ocm_platform::{OverlaySurface, OverlayWindow, Rect, SurfaceHandle};
use raw_window_handle::{
    RawDisplayHandle, RawWindowHandle, WaylandDisplayHandle, WaylandWindowHandle,
};
use std::ptr::NonNull;
use wayland_client::{
    backend::{Backend, ObjectId},
    globals::{registry_queue_init, GlobalList, GlobalListContents},
    protocol::{
        wl_compositor::WlCompositor,
        wl_region::WlRegion,
        wl_registry::WlRegistry,
        wl_shm::WlShm,
        wl_subcompositor::WlSubcompositor,
        wl_surface::WlSurface,
    },
    Connection, Dispatch, Proxy, QueueHandle,
};
use wayland_protocols_wlr::layer_shell::v1::client::{
    zwlr_layer_shell_v1::{Layer as WlrLayer, ZwlrLayerShellV1},
    zwlr_layer_surface_v1::{self, ZwlrLayerSurfaceV1},
};

pub struct WaylandPlatform {
    conn: Connection,
    globals: GlobalList,
    queue: wayland_client::EventQueue<WaylandState>,
}

impl WaylandPlatform {
    pub fn new() -> Result<Self, PlatformError> {
        let conn = Connection::connect_to_env()
            .map_err(|e| PlatformError::Connection(e.to_string()))?;
        let (globals, queue) = registry_queue_init::<WaylandState>(&conn)
            .map_err(|e| PlatformError::Connection(e.to_string()))?;
        Ok(Self { conn, globals, queue })
    }

    pub fn roundtrip(&self) -> Result<(), PlatformError> {
        self.conn
            .roundtrip()
            .map_err(|_| PlatformError::Connection("roundtrip failed".into()))?;
        Ok(())
    }
}

impl ocm_platform::Platform for WaylandPlatform {
    fn create_overlay(&self, config: &Config) -> Result<Box<dyn OverlaySurface>, PlatformError> {
        let qh = self.queue.handle();

        let compositor = self
            .globals
            .bind::<WlCompositor, _, _>(&qh, 1..=1, ())
            .map_err(|_| PlatformError::MissingGlobal("wl_compositor"))?;
        let shm = self
            .globals
            .bind::<WlShm, _, _>(&qh, 1..=1, ())
            .map_err(|_| PlatformError::MissingGlobal("wl_shm"))?;
        let subcompositor = self
            .globals
            .bind::<WlSubcompositor, _, _>(&qh, 1..=1, ())
            .map_err(|_| PlatformError::MissingGlobal("wl_subcompositor"))?;
        let layer_shell = self
            .globals
            .bind::<ZwlrLayerShellV1, _, _>(&qh, 1..=1, ())
            .map_err(|_| PlatformError::MissingGlobal("zwlr_layer_shell_v1"))?;

        let surface = compositor.create_surface(&qh, ());

        let wlr_layer = match config.layer {
            Layer::Overlay => WlrLayer::Overlay,
            Layer::Top => WlrLayer::Top,
        };

        let keyboard_interactivity = match config.keyboard_interactivity {
            KeyboardInteractivity::None => zwlr_layer_surface_v1::KeyboardInteractivity::None,
            KeyboardInteractivity::OnDemand => {
                zwlr_layer_surface_v1::KeyboardInteractivity::OnDemand
            }
        };

        let layer_surface = layer_shell.get_layer_surface(
            &surface,
            None,
            wlr_layer,
            "opencutemate".to_string(),
            &qh,
            LayerSurfaceData::default(),
        );

        layer_surface.set_anchor(zwlr_layer_surface_v1::Anchor::all());
        layer_surface.set_exclusive_zone(-1);
        layer_surface.set_keyboard_interactivity(keyboard_interactivity);

        surface.commit();

        self.conn
            .roundtrip()
            .map_err(|_| PlatformError::Connection("roundtrip failed".into()))?;

        let backend = self.conn.backend();
        let display_ptr = backend.display_ptr();
        let surface_ptr_raw = surface.id().as_ptr();
        let surface_ptr = NonNull::new(surface_ptr_raw)
            .ok_or_else(|| PlatformError::SurfaceCreation("null surface ptr".into()))?;

        let overlay = WaylandOverlay {
            _conn: self.conn.clone(),
            qh,
            surface,
            layer_surface,
            compositor,
            _shm: shm,
            _subcompositor: subcompositor,
            _layer_shell: layer_shell,
            display_ptr,
            surface_ptr,
            logical_size: (1920, 1080),
        };

        Ok(Box::new(overlay))
    }
}

pub struct WaylandOverlay {
    _conn: Connection,
    qh: QueueHandle<WaylandState>,
    surface: WlSurface,
    layer_surface: ZwlrLayerSurfaceV1,
    compositor: WlCompositor,
    _shm: WlShm,
    _subcompositor: WlSubcompositor,
    _layer_shell: ZwlrLayerShellV1,
    display_ptr: *mut wayland_sys::client::wl_display,
    surface_ptr: NonNull<wayland_sys::client::wl_proxy>,
    logical_size: (i32, i32),
}

unsafe impl Send for WaylandOverlay {}
unsafe impl Sync for WaylandOverlay {}

impl SurfaceHandle for WaylandOverlay {
    fn raw_window_handle(&self) -> RawWindowHandle {
        let handle = WaylandWindowHandle::new(self.surface_ptr.cast());
        RawWindowHandle::Wayland(handle)
    }

    fn raw_display_handle(&self) -> RawDisplayHandle {
        let handle = WaylandDisplayHandle::new(
            NonNull::new(self.display_ptr as *mut _).expect("display_ptr is non-null"),
        );
        RawDisplayHandle::Wayland(handle)
    }

    fn logical_size(&self) -> (i32, i32) {
        self.logical_size
    }
}

impl OverlayWindow for WaylandOverlay {
    fn set_input_region(&self, rects: &[Rect]) {
        let region = self.compositor.create_region(&self.qh, ());
        for r in rects {
            region.add(r.x, r.y, r.width, r.height);
        }
        self.surface.set_input_region(Some(&region));
        region.destroy();
        self.surface.commit();
    }

    fn set_click_through(&self, enabled: bool) {
        if enabled {
            let region = self.compositor.create_region(&self.qh, ());
            self.surface.set_input_region(Some(&region));
            region.destroy();
        } else {
            self.surface.set_input_region(None);
        }
        self.surface.commit();
    }
}

impl OverlaySurface for WaylandOverlay {}

#[derive(Default)]
pub struct WaylandState;

#[derive(Default)]
pub struct LayerSurfaceData;

impl Dispatch<WlCompositor, ()> for WaylandState {
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

impl Dispatch<WlShm, ()> for WaylandState {
    fn event(
        _state: &mut Self,
        _proxy: &WlShm,
        _event: <WlShm as Proxy>::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<WlSubcompositor, ()> for WaylandState {
    fn event(
        _state: &mut Self,
        _proxy: &WlSubcompositor,
        _event: <WlSubcompositor as Proxy>::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ZwlrLayerShellV1, ()> for WaylandState {
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

impl Dispatch<ZwlrLayerSurfaceV1, LayerSurfaceData> for WaylandState {
    fn event(
        _state: &mut Self,
        _proxy: &ZwlrLayerSurfaceV1,
        event: <ZwlrLayerSurfaceV1 as Proxy>::Event,
        _data: &LayerSurfaceData,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        if let zwlr_layer_surface_v1::Event::Configure { width, height, .. } = event {
            tracing::info!("layer surface configured: {}x{}", width, height);
        }
    }
}

impl Dispatch<WlSurface, ()> for WaylandState {
    fn event(
        _state: &mut Self,
        _proxy: &WlSurface,
        _event: <WlSurface as Proxy>::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<WlRegion, ()> for WaylandState {
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

impl Dispatch<WlRegistry, GlobalListContents> for WaylandState {
    fn event(
        _state: &mut Self,
        _proxy: &WlRegistry,
        _event: <WlRegistry as Proxy>::Event,
        _data: &GlobalListContents,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}