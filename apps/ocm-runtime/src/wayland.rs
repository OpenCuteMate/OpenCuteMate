//! Linux/Wayland driver: calloop event loop, Wayland platform, Niri
//! desktop-awareness provider, calloop-timer deadline scheduling.
//!
//! Behaviour is intentionally identical to the pre-Phase-3d runtime; the
//! only structural change is that the neutral [`App`] expresses its
//! scheduling as deadlines instead of owning calloop tokens, so the same
//! application core runs on Windows.

use std::cell::Cell;
use std::process::ExitCode;
use std::rc::Rc;
use std::time::Instant;

use calloop::{EventLoop, LoopHandle, RegistrationToken};
use ocm_character::{Character, DesktopBounds};
use ocm_core::{Config, PlatformError};
use ocm_platform::{OverlaySurface, Platform};
use ocm_platform_wayland::WaylandPlatform;
use ocm_renderer::RenderOpts;

use crate::app::{self, App};
use crate::{desktop, RunOptions};

pub(crate) fn run(config: Config, opts: &RunOptions) -> ExitCode {
    // ---- Platform ------------------------------------------------------------
    let mut platform = match WaylandPlatform::connect() {
        Ok(p) => p,
        Err(e @ PlatformError::Connect(_)) => {
            eprintln!("{e}");
            eprintln!("hint: is a Wayland session running (WAYLAND_DISPLAY set)?");
            return ExitCode::FAILURE;
        }
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::FAILURE;
        }
    };

    let params = ocm_platform::OverlayParams {
        layer: config.window.layer,
        keyboard_interactivity: config.window.keyboard_interactivity,
        output: config.window.output.clone(),
        namespace: "opencutemate".to_string(),
    };
    let overlay = match platform.create_overlay(&params) {
        Ok(o) => o,
        Err(e) => {
            eprintln!("failed to create overlay: {e}");
            return ExitCode::FAILURE;
        }
    };

    // ---- Character ------------------------------------------------------------
    let bounds = DesktopBounds::new(overlay.logical_size().0, overlay.logical_size().1);
    let character = Character::new(&config.character, ocm_character::prototype_cube(), bounds);
    tracing::info!(
        phase = character.phase().name(),
        physics = config.character.physics,
        "character initialized"
    );

    // ---- Renderer -------------------------------------------------------------
    let render_opts = RenderOpts {
        vsync: config.render.vsync,
        ..RenderOpts::default()
    };
    let renderer =
        match app::create_renderer(&opts.renderer, &overlay, character.model(), &render_opts) {
            Ok(r) => r,
            Err(message) => {
                eprintln!("{message}");
                drop(overlay);
                drop(platform);
                return ExitCode::FAILURE;
            }
        };
    tracing::info!(backend = %opts.renderer, "renderer initialized");

    // ---- Main loop ---------------------------------------------------------------
    let mut loop_ = match EventLoop::try_new() {
        Ok(l) => l,
        Err(e) => {
            eprintln!("failed to create event loop: {e}");
            drop(renderer);
            drop(overlay);
            drop(platform);
            return ExitCode::FAILURE;
        }
    };
    let handle: LoopHandle<'static, App> = loop_.handle();

    let mut app = App::new(
        Box::new(overlay.clone()),
        renderer,
        config.clone(),
        character,
        bounds,
    );

    // Desktop environment awareness (optional; niri IPC provider).
    let mut desktop = desktop::attach_wayland(&mut app, &handle, &overlay, opts.no_desktop);

    // Initial input region for the initial placement + first render.
    app.start();

    // Wayland event source.
    let source = match platform.into_source() {
        Ok(s) => s,
        Err(e) => {
            eprintln!("failed to prepare the Wayland event source: {e}");
            drop(app); // renderer before platform-owned state
            return ExitCode::FAILURE;
        }
    };
    if let Err(e) = handle.insert_source(source, |event, _, app| {
        app.on_platform_event(event);
    }) {
        eprintln!("failed to register the Wayland event source: {e}");
        drop(app);
        return ExitCode::FAILURE;
    }

    // Scheduler deadlines: one one-shot calloop timer re-armed after each
    // dispatch to whatever the app needs next (none = disarmed, and
    // dispatch(None) parks with zero wakeups).
    let mut deadlines = DeadlineTimer::default();
    deadlines.sync(&handle, &mut app);

    tracing::info!("event loop running");

    let dispatch_result = loop {
        if app.quit {
            break Ok(());
        }
        let result = loop_.dispatch(None, &mut app);
        deadlines.sync(&handle, &mut app);
        // Desktop provider bookkeeping (refresh requests, loss).
        if let Some(provider) = desktop.as_mut() {
            provider.service(&mut app, &handle);
        }
        if let Err(e) = result {
            break Err(e);
        }
    };
    if let Err(ref e) = dispatch_result {
        tracing::error!("event loop error: {e}");
    }

    // ---- Teardown: documented destruction order -----------------------------
    // 1. Drop the app (and with it the renderer: the GPU surface releases
    //    its swapchain *before* any native object dies).
    tracing::info!("teardown: dropping renderer (wgpu surface) first");
    drop(app);
    // 2. Drop the event loop: the Wayland source, its queue and shared state
    //    (the wl_surface and its friends) die last, together with the
    //    connection.
    tracing::info!("teardown: dropping platform/wayland session");
    drop(loop_);
    // 3. The overlay handle is the last Rc into the shared state; dropping
    //    it here (after the loop) guarantees the Wayland objects were alive
    //    for the renderer's entire lifetime.
    drop(overlay);
    drop(desktop);
    tracing::info!("teardown complete");

    if dispatch_result.is_err() {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

/// One-shot calloop timer that always reflects [`App::next_deadline`].
///
/// `fired` distinguishes "the timer source consumed itself" (a stale
/// token must not be removed — calloop recycles source slots) from "a
/// timer is armed and must be replaced/cancelled".
#[derive(Default)]
struct DeadlineTimer {
    armed: Option<(RegistrationToken, Instant)>,
    fired: Rc<Cell<bool>>,
}

impl DeadlineTimer {
    fn sync(&mut self, handle: &LoopHandle<'static, App>, app: &mut App) {
        if self.fired.replace(false) {
            self.armed = None;
        }
        let next = app.next_deadline();
        if self.armed.map(|(_, at)| at) == next {
            return;
        }
        if let Some((token, _)) = self.armed.take() {
            handle.remove(token);
        }
        let Some(at) = next else {
            return;
        };
        let delay = at.saturating_duration_since(Instant::now());
        let fired = self.fired.clone();
        let inserted = handle.insert_source(
            calloop::timer::Timer::from_duration(delay),
            move |_, _, app| {
                fired.set(true);
                app.on_deadline(Instant::now());
                calloop::timer::TimeoutAction::Drop
            },
        );
        match inserted {
            Ok(token) => self.armed = Some((token, at)),
            Err(e) => tracing::error!("failed to schedule the runtime deadline: {e}"),
        }
    }
}
