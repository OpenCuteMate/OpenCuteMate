//! Windows driver: Win32 overlay + message loop with a high-resolution
//! waitable timer, the Win32 desktop-awareness provider, and the same
//! neutral [`App`](crate::app::App) core the Wayland driver runs.
//!
//! Scheduling: the loop waits in `MsgWaitForMultipleObjectsEx` on the
//! deadline timer (armed to `App::next_deadline`) and the provider wake
//! event — `INFINITE` with no deadline, so idle means zero wakeups. There
//! are no compositor frame callbacks on Win32; after each present the app
//! arms its own next frame opportunity, which lands in the same timer.

use std::process::ExitCode;
use std::time::Instant;

use ocm_character::{Character, DesktopBounds};
use ocm_core::Config;
use ocm_platform::{OverlaySurface, Platform};
use ocm_platform_windows::WindowsPlatform;
use ocm_renderer::RenderOpts;

use crate::app::{self, App};
use crate::{desktop, RunOptions};

/// Color made transparent by the Opaque fallback (binary color key).
/// Magenta: far away from the character palette, and the same key the
/// Linux regression suite uses as its test background.
pub(crate) const COLOR_KEY: (u8, u8, u8) = (255, 0, 255);

pub(crate) fn run(config: Config, opts: &RunOptions) -> ExitCode {
    // ---- Platform ------------------------------------------------------------
    let mut platform = match WindowsPlatform::connect() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("failed to initialize the Windows platform: {e}");
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
            eprintln!("failed to create the overlay window: {e}");
            return ExitCode::FAILURE;
        }
    };
    let diag = overlay.diagnostics();
    tracing::info!(
        output = %diag.device,
        physical = ?diag.physical_rect,
        logical = ?diag.logical_size,
        scale = diag.scale,
        "overlay ready"
    );

    // ---- Character ------------------------------------------------------------
    let bounds = DesktopBounds::new(overlay.logical_size().0, overlay.logical_size().1);
    let character = Character::new(&config.character, ocm_character::prototype_cube(), bounds);
    tracing::info!(
        phase = character.phase().name(),
        physics = config.character.physics,
        "character initialized"
    );

    // ---- Renderer -------------------------------------------------------------
    // Windows allows the documented Opaque color-key fallback: some GPU
    // backends (wgpu's DX12 HWND path) offer no compositing alpha.
    let render_opts = RenderOpts {
        vsync: config.render.vsync,
        allow_opaque_fallback: true,
        opaque_clear_color: [
            COLOR_KEY.0 as f32 / 255.0,
            COLOR_KEY.1 as f32 / 255.0,
            COLOR_KEY.2 as f32 / 255.0,
            1.0,
        ],
    };
    let renderer =
        match app::create_renderer(&opts.renderer, &overlay, character.model(), &render_opts) {
            Ok(r) => r,
            Err(message) => {
                eprintln!("{message}");
                drop(overlay);
                return ExitCode::FAILURE;
            }
        };
    tracing::info!(backend = %opts.renderer, "renderer initialized");

    let per_pixel_alpha = renderer.has_per_pixel_alpha();
    if !per_pixel_alpha {
        // The renderer already cleared to COLOR_KEY; make it transparent
        // at the window-system level.
        overlay.set_color_key(COLOR_KEY);
    }

    // ---- App ----------------------------------------------------------------
    let mut app = App::new(
        Box::new(overlay.clone()),
        renderer,
        config.clone(),
        character,
        bounds,
    );
    let mut desktop = desktop::attach_windows(&mut app, &platform, &overlay, opts.no_desktop);
    app.start();

    tracing::info!(
        transparency = if per_pixel_alpha {
            "per-pixel alpha"
        } else {
            "color key"
        },
        "event loop running"
    );

    let result = loop {
        if app.quit {
            break Ok(());
        }

        // Arm the next scheduler deadline (or none: INFINITE wait).
        let now = Instant::now();
        let deadline = app
            .next_deadline()
            .map(|at| at.saturating_duration_since(now));
        if let Err(e) = platform.set_deadline(deadline) {
            tracing::error!("failed to arm the scheduler deadline: {e}");
            break Err(e.to_string());
        }

        // Drain provider events before waiting: an event that arrived while
        // we were rendering is handled immediately.
        if let Some(provider) = desktop.as_mut() {
            provider.poll(&mut app);
        }

        let wake = desktop.as_ref().and_then(|d| d.wake());
        let wait = platform.wait_and_dispatch(wake, |event| app.on_platform_event(event));
        match wait {
            Ok(ocm_platform_windows::Wakeup::Quit) => break Ok(()),
            Ok(_) => {}
            Err(e) => {
                tracing::error!("platform loop error: {e}");
                break Err(e.to_string());
            }
        }

        // Fire scheduler deadlines that are due now.
        app.on_deadline(Instant::now());

        // Provider housekeeping: output re-polls, loss.
        if let Some(provider) = desktop.as_mut() {
            provider.poll(&mut app);
            provider.service(&mut app);
        }
        if app.take_desktop_stop_request() {
            desktop = None;
        }
    };

    // ---- Teardown: renderer (inside the app) before the platform window --
    tracing::info!("teardown: dropping renderer (wgpu surface) first");
    drop(app);
    tracing::info!("teardown: dropping platform/window");
    drop(desktop);
    drop(platform);
    tracing::info!("teardown complete");

    if result.is_err() {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}
