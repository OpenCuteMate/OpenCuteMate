//! Platform seam for desktop environment awareness.
//!
//! The shared desktop model ([`ocm_desktop`]) and the
//! [`DesktopProvider`](ocm_desktop::DesktopProvider) contract are
//! platform-neutral; what is per-platform is (a) which provider to
//! construct and (b) how its event source joins the runtime's loop.
//! This module is that seam: one cfg-gated implementation per
//! platform, one neutral call site in `main`.
//!
//! | Platform | Provider | Event delivery | Status |
//! |---|---|---|---|
//! | Linux (Wayland) | `ocm-desktop-niri` (IPC socket) | calloop source over the socket fd | implemented, suite-validated |
//! | Windows | `ocm-desktop-windows` (Win32 event hooks) | own hook thread + message pump; drained by `poll_events` | provider implemented (validated standalone via `win-probe`); the runtime is **not** Windows-hostable yet |
//!
//! The runtime binary itself remains Wayland-hosted (layer-shell
//! surface, calloop loop, wgpu/Bevy over Vulkan-Wayland). Running the
//! character app on Windows additionally needs: a top-level layered
//! window behind `ocm-platform`, a Win32 message loop implementing the
//! same event-source contract (or a calloop Win32 backend), and
//! DXGI/`raw-window-handle` surface wiring in the renderer — the
//! documented Phase 4+ porting surface, out of scope for this phase.
//! Windows desktop awareness is validated standalone via the
//! `win-probe` example in `ocm-desktop-windows`.

use ocm_platform_wayland::WaylandOverlay;

use crate::App;

/// Attach desktop awareness to the running app, or log why it is not
/// available. Failure is never fatal: the app runs exactly as the
/// platform baseline without awareness.
pub(crate) fn attach(
    app: &mut App,
    handle: &calloop::LoopHandle<'static, App>,
    overlay: &WaylandOverlay,
    disabled: bool,
) {
    if disabled {
        tracing::info!("desktop awareness disabled (--no-desktop)");
        return;
    }
    attach_platform(app, handle, overlay);
}

#[cfg(target_os = "linux")]
fn attach_platform(
    app: &mut App,
    handle: &calloop::LoopHandle<'static, App>,
    overlay: &WaylandOverlay,
) {
    use ocm_desktop::{DesktopEnvironment, DesktopProvider, EnvironmentEvent};
    use ocm_desktop_niri::{NiriProvider, NiriSource};
    use ocm_platform::OverlaySurface;

    // Additive by design: without a provider the runtime is exactly the
    // validated Phase 3a app. The provider feeds snapshots and events;
    // none of them schedule rendering on their own — only a real change
    // to the character's output geometry can touch the character.
    match NiriProvider::connect(None) {
        Ok(mut provider) => {
            let mut environment = DesktopEnvironment::new(provider.capabilities());
            match provider.snapshot() {
                Ok(snapshot) => {
                    tracing::info!(
                        outputs = snapshot.outputs.len(),
                        windows = snapshot.windows.len(),
                        workspaces = snapshot.workspaces.len(),
                        "desktop environment snapshot acquired (niri IPC)"
                    );
                    // The first snapshot establishes the environment;
                    // nothing is scheduled from it (bounds come from the
                    // layer-shell configure until they differ).
                    let _ = environment.apply(EnvironmentEvent::Snapshot(snapshot));
                }
                Err(e) => tracing::warn!("desktop snapshot failed: {e}"),
            }
            match NiriSource::new(provider) {
                Ok(source) => {
                    match handle.insert_source(source, |event: EnvironmentEvent, _, app| {
                        app.on_environment_event(event);
                    }) {
                        Ok(token) => {
                            let target = overlay.target_output().unwrap_or_default();
                            app.desktop = Some(crate::DesktopState {
                                environment,
                                target_output: target,
                                source: Some(token),
                                output_refresh: None,
                                last_window_report: (0, 0),
                            });
                            app.arm_output_refresh();
                            tracing::info!("desktop awareness attached (niri event stream)");
                        }
                        Err(e) => tracing::warn!("failed to register the niri source: {e}"),
                    }
                }
                Err(e) => tracing::warn!("failed to prepare the niri event source: {e}"),
            }
        }
        Err(e) => {
            tracing::info!("desktop awareness unavailable ({e}); continuing without it");
        }
    }
}

#[cfg(not(target_os = "linux"))]
fn attach_platform(
    app: &mut App,
    _handle: &calloop::LoopHandle<'static, App>,
    _overlay: &WaylandOverlay,
) {
    // No provider wiring for this platform in the runtime yet (see the
    // module docs for the Windows porting surface). The provider crate
    // itself is available and validated via its own diagnostic example.
    let _ = app;
    tracing::info!(
        "desktop awareness: no runtime provider for this platform; \
         the character runs without environment data"
    );
}
