//! [`WaylandPlatform`]: connection setup, global discovery, monitor
//! enumeration and layer-shell surface creation.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use ocm_core::{KeyboardInteractivity, Layer, PlatformError};
use ocm_platform::{MonitorInfo, OverlayParams, OverlaySurface, Platform};
use wayland_client::globals::{registry_queue_init, GlobalList};
use wayland_client::protocol::{wl_compositor::WlCompositor, wl_output::WlOutput, wl_seat::WlSeat};
use wayland_client::{Connection, EventQueue, QueueHandle};
use wayland_protocols::wp::fractional_scale::v1::client::wp_fractional_scale_manager_v1::WpFractionalScaleManagerV1;
use wayland_protocols::wp::viewporter::client::wp_viewporter::WpViewporter;
use wayland_protocols::xdg::xdg_output::zv1::client::zxdg_output_manager_v1::ZxdgOutputManagerV1;
use wayland_protocols_wlr::layer_shell::v1::client::{
    zwlr_layer_shell_v1::{self, ZwlrLayerShellV1},
    zwlr_layer_surface_v1,
};

use crate::overlay::WaylandOverlay;
use crate::source::WaylandSource;
use crate::state::{DispatchState, OutputData, OutputRecord, Shared};

/// Native Wayland platform backend.
pub struct WaylandPlatform {
    conn: Connection,
    globals: GlobalList,
    queue: EventQueue<DispatchState>,
    state: DispatchState,
}

impl Platform for WaylandPlatform {
    type Overlay = WaylandOverlay;
    type Error = PlatformError;

    fn connect() -> Result<Self, PlatformError> {
        let conn =
            Connection::connect_to_env().map_err(|e| PlatformError::Connect(e.to_string()))?;
        let (globals, queue) = registry_queue_init::<DispatchState>(&conn)
            .map_err(|e| PlatformError::Connect(e.to_string()))?;
        let qh: QueueHandle<DispatchState> = queue.handle();
        let shared = Rc::new(RefCell::new(Shared::new(conn.clone(), qh)));
        let state = DispatchState { shared };
        let mut me = Self {
            conn,
            globals,
            queue,
            state,
        };

        me.log_globals();
        me.discover_monitors()?;

        Ok(me)
    }

    fn monitors(&self) -> Vec<MonitorInfo> {
        self.state
            .shared
            .borrow()
            .outputs
            .iter()
            .map(|record| record.monitor.clone())
            .collect()
    }

    fn create_overlay(&mut self, params: &OverlayParams) -> Result<WaylandOverlay, PlatformError> {
        let qh = self.queue.handle();
        let shared = self.state.shared.clone();

        let compositor: WlCompositor = self
            .globals
            .bind(&qh, 1..=4, ())
            .map_err(|_| PlatformError::MissingGlobal("wl_compositor"))?;
        let layer_shell: ZwlrLayerShellV1 = self
            .globals
            .bind(&qh, 1..=4, ())
            .map_err(|_| PlatformError::MissingGlobal("zwlr_layer_shell_v1"))?;

        // Target output: by name when given, first advertised otherwise.
        let output = {
            let s = shared.borrow();
            choose_output(&s.outputs, &params.output)
                .cloned()
                .ok_or_else(|| PlatformError::OutputNotFound(params.output.clone()))?
        };
        tracing::info!(output = %output.monitor.name, "target output selected");

        let surface = compositor.create_surface(&qh, ());

        let wlr_layer = match params.layer {
            Layer::Overlay => zwlr_layer_shell_v1::Layer::Overlay,
            Layer::Top => zwlr_layer_shell_v1::Layer::Top,
        };
        let kbd_interactivity = match params.keyboard_interactivity {
            KeyboardInteractivity::None => zwlr_layer_surface_v1::KeyboardInteractivity::None,
            KeyboardInteractivity::OnDemand => {
                zwlr_layer_surface_v1::KeyboardInteractivity::OnDemand
            }
            KeyboardInteractivity::Exclusive => {
                zwlr_layer_surface_v1::KeyboardInteractivity::Exclusive
            }
        };

        let layer_surface = layer_shell.get_layer_surface(
            &surface,
            Some(&output.proxy),
            wlr_layer,
            params.namespace.clone(),
            &qh,
            (),
        );

        // Full-output overlay: anchor all four edges, size assigned by the
        // compositor. Exclusive zone -1 = ignore other layers' reservations.
        layer_surface.set_anchor(zwlr_layer_surface_v1::Anchor::all());
        layer_surface.set_exclusive_zone(-1);
        layer_surface.set_keyboard_interactivity(kbd_interactivity);
        layer_surface.set_size(0, 0);

        // The surface is never opaque anywhere (the protocol default is an
        // empty opaque region; make it explicit).
        {
            let region = compositor.create_region(&qh, ());
            surface.set_opaque_region(Some(&region));
            region.destroy();
        }

        // Fractional scale + viewporter, when the compositor offers them.
        let fractional = self
            .globals
            .bind::<WpFractionalScaleManagerV1, _, _>(&qh, 1..=1, ())
            .ok()
            .inspect(|_| tracing::debug!("wp_fractional_scale_manager_v1 available"))
            .map(|manager| manager.get_fractional_scale(&surface, &qh, ()));
        let viewport = self
            .globals
            .bind::<WpViewporter, _, _>(&qh, 1..=1, ())
            .ok()
            .inspect(|_| tracing::debug!("wp_viewporter available"))
            .map(|viewporter| viewporter.get_viewport(&surface, &qh, ()));

        {
            let mut s = shared.borrow_mut();
            s.compositor = Some(compositor);
            s.surface = Some(surface.clone());
            s.layer_surface = Some(layer_surface);
            s.viewport = viewport;
            s.fractional = fractional;
        }

        // Initial commit: tells the compositor we are done with initial
        // state so it can send the first configure. No buffer is attached
        // yet; the surface is not mapped until the first present.
        surface.commit();
        if let Err(e) = self.conn.flush() {
            tracing::warn!("wayland flush after initial commit: {e}");
        }

        // Block until the first configure (bounded), so the overlay reports
        // real sizes to the renderer from the start.
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            {
                let s = shared.borrow();
                if s.closed {
                    return Err(PlatformError::Closed);
                }
                if s.logical_size.is_some() {
                    break;
                }
            }
            if Instant::now() > deadline {
                return Err(PlatformError::ConfigureTimeout);
            }
            self.queue
                .roundtrip(&mut self.state)
                .map_err(|e| PlatformError::Protocol(e.to_string()))?;
        }

        // Scale is now known (fractional-scale arrives with the roundtrips
        // above). Apply it before any buffer exists.
        {
            let mut s = shared.borrow_mut();
            s.apply_scale();
            s.note_scale();
            // Initial state was consumed synchronously; the runtime reads it
            // through the overlay handle instead of replayed events.
            s.pending.clear();
        }
        let _ = self.conn.flush();

        let overlay = WaylandOverlay { shared };
        tracing::info!(
            logical = ?overlay.logical_size(),
            buffer = ?overlay.buffer_size(),
            scale = overlay.scale_factor(),
            "overlay surface ready"
        );
        Ok(overlay)
    }
}

impl WaylandPlatform {
    /// Log every advertised global with its version (protocol evidence).
    fn log_globals(&self) {
        self.globals.contents().with_list(|globals| {
            for global in globals {
                tracing::debug!(name = global.name, interface = %global.interface, version = global.version, "global");
            }
        });
    }

    /// Bind `wl_seat` and enumerate `wl_output` globals (with xdg-output
    /// details when available). Two roundtrips: bind → events → done.
    fn discover_monitors(&mut self) -> Result<(), PlatformError> {
        let qh = self.queue.handle();
        let shared = self.state.shared.clone();

        if let Ok(seat) = self.globals.bind::<WlSeat, _, _>(&qh, 1..=6, ()) {
            shared.borrow_mut().seat = Some(seat);
        } else {
            tracing::warn!("no wl_seat global; pointer input unavailable");
        }

        // Enumerate wl_output globals and bind each one BY NAME. This is
        // required for multi-instance globals: `GlobalList::bind` only binds
        // the first global of an interface (per its own documentation), which
        // would mis-identify monitors on multi-output systems.
        let mut output_globals: Vec<(u32, u32)> = Vec::new();
        self.globals.contents().with_list(|globals| {
            for global in globals {
                if global.interface == "wl_output" {
                    output_globals.push((global.name, global.version));
                }
            }
        });

        let registry = self.globals.registry().clone();
        let mut records: Vec<OutputRecord> = Vec::new();
        for (global_name, advertised_version) in output_globals {
            let index = records.len();
            // Bind at min(advertised, 4): v4 gives name/description events.
            let version = advertised_version.min(4);
            let proxy: WlOutput = registry.bind(global_name, version, &qh, OutputData { index });
            records.push(OutputRecord {
                proxy,
                monitor: MonitorInfo {
                    name: format!("wl_output@{global_name}"),
                    description: String::new(),
                    logical_position: None,
                    logical_size: None,
                    scale: 1,
                },
            });
        }

        // xdg-output for friendly names and logical geometry.
        if let Ok(manager) = self
            .globals
            .bind::<ZxdgOutputManagerV1, _, _>(&qh, 1..=3, ())
        {
            for (index, record) in records.iter().enumerate() {
                manager.get_xdg_output(&record.proxy, &qh, OutputData { index });
            }
        }

        shared.borrow_mut().outputs = records;

        // Roundtrip twice: bind results + xdg-output events.
        for _ in 0..2 {
            self.queue
                .roundtrip(&mut self.state)
                .map_err(|e| PlatformError::Protocol(e.to_string()))?;
        }

        for record in shared.borrow().outputs.iter() {
            tracing::info!(
                name = %record.monitor.name,
                description = %record.monitor.description,
                scale = record.monitor.scale,
                logical = ?record.monitor.logical_size,
                "monitor"
            );
        }
        Ok(())
    }

    /// Consume the platform into a calloop event source for the main loop.
    ///
    /// The returned source owns the event queue, dispatch state and a
    /// duplicate of the socket fd for readiness polling. The overlay handles
    /// created earlier remain valid: they hold their own `Rc` into the
    /// shared state, which now lives inside the source.
    pub fn into_source(self) -> Result<WaylandSource, PlatformError> {
        WaylandSource::new(self.conn, self.queue, self.state)
            .map_err(|e| PlatformError::Other(format!("poll fd setup: {e}")))
    }
}

fn choose_output<'a>(outputs: &'a [OutputRecord], wanted: &str) -> Option<&'a OutputRecord> {
    if outputs.is_empty() {
        return None;
    }
    if wanted.is_empty() {
        return Some(&outputs[0]);
    }
    outputs
        .iter()
        .find(|record| record.monitor.name == wanted || record.monitor.description.contains(wanted))
        .or(Some(&outputs[0]))
}
