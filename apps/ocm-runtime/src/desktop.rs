//! Platform seam for desktop environment awareness.
//!
//! The shared desktop model ([`ocm_desktop`]) and the
//! [`DesktopProvider`](ocm_desktop::DesktopProvider) contract are
//! platform-neutral; what is per-platform is which provider to construct
//! and how its events reach the runtime loop:
//!
//! | Platform | Provider | Delivery |
//! |---|---|---|
//! | Linux (Wayland) | `ocm-desktop-niri` (IPC socket) | calloop source over the socket fd |
//! | Windows | `ocm-desktop-windows` (Win32 event hooks) | provider hook thread + wake event; drained by `poll_events` |
//!
//! Each driver owns its provider (the transport differs fundamentally:
//! a file descriptor vs. an OS wake handle), while [`crate::app::App`]
//! only ever sees neutral [`ocm_desktop::EnvironmentEvent`]s.

#![cfg_attr(not(any(target_os = "linux", target_os = "windows")), allow(dead_code))]

// ---------------------------------------------------------------------------
// Linux: Niri IPC provider + calloop source
// ---------------------------------------------------------------------------

#[cfg(target_os = "linux")]
pub(crate) use linux::WaylandDesktop;

#[cfg(target_os = "linux")]
pub(crate) fn attach_wayland(
    app: &mut crate::app::App,
    handle: &calloop::LoopHandle<'static, crate::app::App>,
    overlay: &ocm_platform_wayland::WaylandOverlay,
    disabled: bool,
) -> Option<WaylandDesktop> {
    linux::attach(app, handle, overlay, disabled)
}

#[cfg(target_os = "linux")]
mod linux {
    use std::cell::Cell;
    use std::rc::Rc;

    use calloop::{LoopHandle, RegistrationToken};
    use ocm_desktop::{DesktopEnvironment, DesktopProvider, EnvironmentEvent};
    use ocm_desktop_niri::{NiriProvider, NiriSource};
    use ocm_platform::OverlaySurface;

    use crate::app::{App, DesktopState};

    /// Driver-owned desktop provider state for the Wayland runtime.
    pub(crate) struct WaylandDesktop {
        /// The event source token, while the source is still registered.
        source: Option<RegistrationToken>,
        /// Set by the source callback when it saw `ProviderLost`: the
        /// source removes itself (`PostAction::Remove`), so the token is
        /// stale and must not be removed again.
        source_self_removed: Rc<Cell<bool>>,
    }

    pub(crate) fn attach(
        app: &mut App,
        handle: &LoopHandle<'static, App>,
        overlay: &ocm_platform_wayland::WaylandOverlay,
        disabled: bool,
    ) -> Option<WaylandDesktop> {
        if disabled {
            tracing::info!("desktop awareness disabled (--no-desktop)");
            return None;
        }
        // Additive by design: without a provider the runtime is exactly the
        // validated Phase 3a app. The provider feeds snapshots and events;
        // none of them schedule rendering on their own — only a real change
        // to the character's output geometry can touch the character.
        let mut provider = match NiriProvider::connect(None) {
            Ok(p) => p,
            Err(e) => {
                tracing::info!("desktop awareness unavailable ({e}); continuing without it");
                return None;
            }
        };

        let mut environment = DesktopEnvironment::new(provider.capabilities());
        match provider.snapshot() {
            Ok(snapshot) => {
                tracing::info!(
                    outputs = snapshot.outputs.len(),
                    windows = snapshot.windows.len(),
                    workspaces = snapshot.workspaces.len(),
                    "desktop environment snapshot acquired (niri IPC)"
                );
                // The first snapshot establishes the environment; nothing
                // is scheduled from it (bounds come from the layer-shell
                // configure until they differ).
                let _ = environment.apply(EnvironmentEvent::Snapshot(snapshot));
            }
            Err(e) => tracing::warn!("desktop snapshot failed: {e}"),
        }

        let source = match NiriSource::new(provider) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!("failed to prepare the niri event source: {e}");
                return None;
            }
        };

        let self_removed = Rc::new(Cell::new(false));
        let flag = self_removed.clone();
        match handle.insert_source(source, move |event: EnvironmentEvent, _, app: &mut App| {
            if matches!(event, EnvironmentEvent::ProviderLost) {
                flag.set(true);
            }
            app.on_environment_event(event);
        }) {
            Ok(token) => {
                app.desktop = Some(DesktopState {
                    environment,
                    target_output: overlay.target_output().unwrap_or_default(),
                    refresh_deadline: None,
                    last_window_report: (0, 0),
                });
                app.arm_output_refresh();
                tracing::info!("desktop awareness attached (niri event stream)");
                Some(WaylandDesktop {
                    source: Some(token),
                    source_self_removed: self_removed,
                })
            }
            Err(e) => {
                tracing::warn!("failed to register the niri source: {e}");
                None
            }
        }
    }

    impl WaylandDesktop {
        /// Handle scheduler requests after each loop dispatch: output
        /// re-polls (the event stream has no output events) and provider
        /// loss cleanup.
        pub(crate) fn service(&mut self, app: &mut App, handle: &LoopHandle<'static, App>) {
            if app.take_desktop_refresh_request() {
                let change = match NiriProvider::fetch_outputs(None) {
                    Ok(list) => EnvironmentEvent::OutputsChanged(list),
                    Err(e) => {
                        tracing::warn!("output re-poll failed: {e}");
                        EnvironmentEvent::ProviderLost
                    }
                };
                app.on_environment_event(change);
                app.arm_output_refresh();
            }
            if app.take_desktop_stop_request() {
                if !self.source_self_removed.get() {
                    if let Some(token) = self.source.take() {
                        handle.remove(token);
                    }
                } else {
                    self.source = None;
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Windows: Win32 provider + wake handle
// ---------------------------------------------------------------------------

#[cfg(windows)]
pub(crate) use win::WindowsDesktop;

#[cfg(windows)]
pub(crate) fn attach_windows(
    app: &mut crate::app::App,
    platform: &ocm_platform_windows::WindowsPlatform,
    overlay: &ocm_platform_windows::WindowsOverlay,
    disabled: bool,
) -> Option<WindowsDesktop> {
    win::attach(app, platform, overlay, disabled)
}

#[cfg(windows)]
mod win {
    use ocm_desktop::{DesktopEnvironment, DesktopProvider, EnvironmentEvent};
    use ocm_desktop_windows::WindowsProvider;
    use ocm_platform::OverlaySurface;
    use ocm_platform_windows::{WakeHandle, WindowsOverlay, WindowsPlatform};

    use crate::app::{App, DesktopState};

    /// Driver-owned desktop provider state for the Windows runtime.
    pub(crate) struct WindowsDesktop {
        provider: WindowsProvider,
        /// Kept alive for the provider's hook thread (its callback
        /// signals this event).
        _wake: Option<WakeHandle>,
    }

    pub(crate) fn attach(
        app: &mut App,
        platform: &WindowsPlatform,
        overlay: &WindowsOverlay,
        disabled: bool,
    ) -> Option<WindowsDesktop> {
        if disabled {
            tracing::info!("desktop awareness disabled (--no-desktop)");
            return None;
        }
        let mut provider = match WindowsProvider::connect() {
            Ok(p) => p,
            Err(e) => {
                tracing::info!("desktop awareness unavailable ({e}); continuing without it");
                return None;
            }
        };

        // The provider pushes events from its hook thread; the wake event
        // makes the blocking wait return so they are drained promptly
        // (no polling).
        let wake = match platform.wake_handle() {
            Ok(w) => {
                provider.set_wake_callback(w.callback());
                Some(w)
            }
            Err(e) => {
                tracing::warn!(
                    "provider wake event unavailable ({e}); events drain on other wakeups"
                );
                None
            }
        };

        let mut environment = DesktopEnvironment::new(provider.capabilities());
        match provider.snapshot() {
            Ok(snapshot) => {
                tracing::info!(
                    outputs = snapshot.outputs.len(),
                    windows = snapshot.windows.len(),
                    "desktop environment snapshot acquired (win32)"
                );
                let _ = environment.apply(EnvironmentEvent::Snapshot(snapshot));
            }
            Err(e) => tracing::warn!("desktop snapshot failed: {e}"),
        }

        app.desktop = Some(DesktopState {
            environment,
            target_output: overlay.target_output().unwrap_or_default(),
            refresh_deadline: None,
            last_window_report: (0, 0),
        });
        app.arm_output_refresh();
        tracing::info!("desktop awareness attached (win32 event hooks)");
        Some(WindowsDesktop {
            provider,
            _wake: wake,
        })
    }

    impl WindowsDesktop {
        /// Drain pushed provider events (called after every loop wake).
        pub(crate) fn poll(&mut self, app: &mut App) {
            let events = match self.provider.poll_events() {
                Ok(events) => events,
                Err(e) => {
                    tracing::warn!("desktop provider poll failed: {e}");
                    app.on_environment_event(EnvironmentEvent::ProviderLost);
                    return;
                }
            };
            for event in events {
                app.on_environment_event(event);
            }
        }

        /// One-shot output re-fetch (scheduler-driven, as on Linux).
        pub(crate) fn service(&mut self, app: &mut App) {
            if app.take_desktop_refresh_request() {
                let change = match self.provider.refresh_outputs() {
                    Ok(list) => EnvironmentEvent::OutputsChanged(list),
                    Err(e) => {
                        tracing::warn!("output re-poll failed: {e}");
                        EnvironmentEvent::ProviderLost
                    }
                };
                app.on_environment_event(change);
                app.arm_output_refresh();
            }
            if app.take_desktop_stop_request() {
                tracing::warn!("desktop provider stopped; awareness paused");
            }
        }

        /// The platform wake handle for the loop wait, when available.
        pub(crate) fn wake(&self) -> Option<&WakeHandle> {
            self._wake.as_ref()
        }
    }
}
