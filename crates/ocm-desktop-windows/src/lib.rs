//! # ocm-desktop-windows
//!
//! Desktop environment provider for Microsoft Windows, implementing the
//! neutral [`ocm_desktop::DesktopProvider`] contract with **native
//! Win32 APIs** — no subprocesses, no PowerShell, no X11-style
//! translation layers.
//!
//! ## What it reads (and with which APIs)
//!
//! | Data | API |
//! |---|---|
//! | Monitors, rects, work areas, primary | `EnumDisplayMonitors`, `GetMonitorInfoW` |
//! | Mode (resolution, refresh, orientation) | `EnumDisplaySettingsW` |
//! | Per-monitor DPI | `GetDpiForMonitor` (MDT_EFFECTIVE_DPI) |
//! | Windows, titles, visibility, states | `EnumWindows`, `IsWindowVisible`, `IsIconic`, `IsZoomed`, `GetWindowTextW`, `GetWindowLongPtrW` |
//! | Real visible window bounds | `DwmGetWindowAttribute` (DWMWA_EXTENDED_FRAME_BOUNDS), `GetWindowRect` fallback |
//! | Suspended/cloaked detection | `DwmGetWindowAttribute` (DWMWA_CLOAKED) |
//! | Process identity | `GetWindowThreadProcessId`, `OpenProcess` + `QueryFullProcessImageNameW` |
//! | Focus | `GetForegroundWindow`, `EVENT_SYSTEM_FOREGROUND` |
//! | Change events | `SetWinEventHook` (WINEVENT_OUTOFCONTEXT) pumped on a dedicated thread |
//!
//! Geometry is read under a per-thread
//! `DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2` context so all rects
//! are true physical pixels; the pure translation layer converts them
//! to the neutral global-logical space (scale = DPI ÷ 96, exact
//! division). See `translate.rs` and `docs/desktop-awareness.md`.
//!
//! ## Truthfulness
//!
//! - Fullscreen state is **not** detected (comparing rects to monitor
//!   bounds is a heuristic): `WindowState::fullscreen` stays `None`.
//! - Virtual desktops: there is no documented API to associate
//!   third-party windows with virtual desktops; the
//!   `IVirtualDesktopManager` interface is documented only for the
//!   caller's own windows and fails for foreign ones. Workspaces stay
//!   unavailable in this provider (documented, not faked).
//! - Window filtering is explicit and documented (see
//!   `translate::window_to_info`): invisible, cloaked (suspended UWP)
//!   and `WS_EX_TOOLWINDOW` windows are excluded; everything else a
//!   `EnumWindows` pass reports is kept.
//!
//! ## Platform gating
//!
//! The whole Win32 layer is `#[cfg(windows)]`. On other platforms the
//! provider reports [`ocm_desktop::ProviderError::Unavailable`] and the
//! pure translation layer + its fixture tests still compile and run, so
//! the shared model logic is exercised on every OS. Windows builds are
//! type-verified via cross-compilation (`tests/windows-crosscheck.sh`)
//! and validated on real hardware by the user with the `win-probe`
//! example.

mod api;
#[cfg(test)]
mod fixtures;
mod translate;

use ocm_desktop::{
    DesktopCapabilities, DesktopProvider, DesktopSnapshot, EnvironmentEvent, OutputInfo,
    ProviderError,
};

/// Environment provider for Microsoft Windows.
///
/// The provider owns a dedicated event-hook thread (Win32 requires a
/// message pump for `SetWinEventHook` callbacks); everything else runs
/// on the calling thread. Teardown posts `WM_QUIT` to the hook thread
/// and joins it.
pub struct WindowsProvider {
    /// cfg(windows) state; `None` on other platforms (the provider is
    /// then permanently unavailable).
    #[cfg(windows)]
    inner: Option<provider::State>,
}

impl WindowsProvider {
    /// Connect to the local Windows session. Returns
    /// [`ProviderError::Unavailable`] on non-Windows platforms (the
    /// awareness stays optional, exactly like a missing Niri socket).
    pub fn connect() -> Result<Self, ProviderError> {
        #[cfg(windows)]
        {
            Ok(Self {
                inner: Some(provider::State::connect()?),
            })
        }
        #[cfg(not(windows))]
        {
            Err(ProviderError::Unavailable)
        }
    }
}

impl DesktopProvider for WindowsProvider {
    fn name(&self) -> &'static str {
        "win32"
    }

    fn capabilities(&self) -> DesktopCapabilities {
        // Verified facts only; see docs/desktop-awareness.md §8 for the
        // matrix and what "implemented, pending Windows validation"
        // means.
        DesktopCapabilities {
            outputs: true,
            output_logical_geometry: true,
            output_scale: true,
            output_mode: true,
            output_transform: true,
            // Real work areas (rcWork): taskbar-reserved regions.
            work_areas: true,
            primary_output: true,
            // No monitor-change events in this provider; the hook
            // thread re-enumerates on a WM_TIMER (10 s).
            output_events: false,

            windows: true,
            window_identity: true,
            // Virtual desktops are not detectable for foreign windows
            // via documented APIs.
            window_workspace: false,
            window_focus: true,
            window_geometry: true,
            window_geometry_all: true,
            window_visibility: true,
            window_min_max_state: true,
            // Rect-vs-monitor comparison would be a heuristic: refused.
            window_fullscreen_state: false,

            workspaces: false,
            workspace_events: false,
            focus_events: true,
            event_stream: true,
        }
    }

    fn snapshot(&mut self) -> Result<DesktopSnapshot, ProviderError> {
        #[cfg(windows)]
        match &mut self.inner {
            Some(state) => state.snapshot(),
            None => Err(ProviderError::Unavailable),
        }
        #[cfg(not(windows))]
        Err(ProviderError::Unavailable)
    }

    fn poll_events(&mut self) -> Result<Vec<EnvironmentEvent>, ProviderError> {
        #[cfg(windows)]
        match &mut self.inner {
            Some(state) => state.poll_events(),
            None => Err(ProviderError::Unavailable),
        }
        #[cfg(not(windows))]
        Err(ProviderError::Unavailable)
    }

    fn refresh_outputs(&mut self) -> Result<Vec<OutputInfo>, ProviderError> {
        #[cfg(windows)]
        match &mut self.inner {
            Some(state) => state.refresh_outputs(),
            None => Err(ProviderError::Unavailable),
        }
        #[cfg(not(windows))]
        Err(ProviderError::Unavailable)
    }
}

/// Bridge the API-layer error into the neutral provider error.
#[cfg(windows)]
fn api_error(error: api::ApiError) -> ProviderError {
    match error {
        api::ApiError::Call(m) => ProviderError::Protocol(m),
    }
}

mod provider {
    //! cfg(windows) provider state: monitor + window snapshot, the
    //! event hook thread, and the raw-event → neutral-event mapping.

    #![cfg(windows)]

    use std::collections::BTreeMap;
    use std::sync::mpsc;

    use super::api_error;
    use ocm_desktop::{DesktopSnapshot, EnvironmentEvent, OutputInfo, ProviderError, WindowInfo};

    use crate::{api, translate};

    /// Monitor re-enumeration tick, riding the event pump. Windows has
    /// no push notification for monitor changes available to a plain
    /// client; polling here is required and documented (see
    /// docs/desktop-awareness.md §6). Monitor changes are rare events;
    /// one enumeration (~0.1 ms) per tick is negligible.
    const MONITOR_TICK_MS: u32 = 10_000;

    /// Events buffered between polls. Sized for window-drag storms
    /// (location-change events fire per frame); a slow consumer drops
    /// overflow (logged once by the hook thread), never blocks it.
    const EVENT_CHANNEL_BOUND: usize = 4096;

    pub struct State {
        /// Last known window records: the event path needs full
        /// records, addition detection and removal detection.
        windows: BTreeMap<u64, WindowInfo>,
        /// Per-pid executable names (queried once per process).
        exe_cache: api::ExeCache,
        events: mpsc::Receiver<api::RawEvent>,
        hook: Option<api::HookHandle>,
    }

    impl State {
        pub fn connect() -> Result<Self, ProviderError> {
            // Monitor discovery doubles as the availability probe: a
            // session with zero monitors cannot host awareness.
            let monitors = api::monitors().map_err(api_error)?;
            if monitors.is_empty() {
                return Err(ProviderError::Unavailable);
            }
            tracing::debug!(monitors = monitors.len(), "win32 monitor discovery ok");

            let (sender, events) = mpsc::sync_channel(EVENT_CHANNEL_BOUND);
            // A failed hook setup surfaces here: connect fails, the
            // caller treats awareness as unavailable (never fatal).
            let hook = api::start_event_thread(sender, MONITOR_TICK_MS).map_err(api_error)?;
            tracing::debug!("win32 event hook running");

            Ok(Self {
                windows: BTreeMap::new(),
                exe_cache: BTreeMap::new(),
                events,
                hook: Some(hook),
            })
        }

        pub fn snapshot(&mut self) -> Result<DesktopSnapshot, ProviderError> {
            let raw_windows = api::enumerate_windows(&mut self.exe_cache).map_err(api_error)?;
            let windows: Vec<WindowInfo> = raw_windows
                .iter()
                .filter_map(translate::window_to_info)
                .collect();
            self.windows = windows.iter().map(|w| (w.id, w.clone())).collect();
            tracing::debug!(windows = windows.len(), "win32 window enumeration ok");
            Ok(DesktopSnapshot {
                outputs: translate::monitors_to_info(&api::monitors().map_err(api_error)?),
                windows,
                workspaces: Vec::new(),
            })
        }

        pub fn poll_events(&mut self) -> Result<Vec<EnvironmentEvent>, ProviderError> {
            // Drain the raw events, coalescing per-window actions so a
            // drag storm costs one re-query per window per poll.
            let mut refresh: Vec<u64> = Vec::new();
            let mut removed: Vec<u64> = Vec::new();
            let mut focus: Option<u64> = None;
            let mut monitor_refresh = false;

            while let Ok(raw) = self.events.try_recv() {
                match translate::map_win_event(&raw) {
                    translate::EventAction::Ignore => {}
                    translate::EventAction::RefreshWindow => refresh.push(raw.hwnd),
                    translate::EventAction::WindowRemoved => removed.push(raw.hwnd),
                    translate::EventAction::Focused => focus = Some(raw.hwnd),
                    translate::EventAction::MonitorRefresh => monitor_refresh = true,
                }
            }

            let mut events: Vec<EnvironmentEvent> = Vec::new();

            if monitor_refresh {
                let outputs = translate::monitors_to_info(&api::monitors().map_err(api_error)?);
                events.push(EnvironmentEvent::OutputsChanged(outputs));
            }
            // Removals first, so a destroyed window cannot be
            // resurrected by a queued create in the same batch.
            for hwnd in &removed {
                events.extend(self.apply_removal(*hwnd));
            }
            refresh.sort_unstable();
            refresh.dedup();
            for hwnd in refresh {
                if removed.contains(&hwnd) {
                    continue;
                }
                events.extend(self.apply_refresh(hwnd));
            }
            if let Some(hwnd) = focus {
                events.push(EnvironmentEvent::WindowFocused {
                    id: (hwnd != 0).then_some(hwnd),
                });
            }
            Ok(events)
        }

        pub fn refresh_outputs(&mut self) -> Result<Vec<OutputInfo>, ProviderError> {
            Ok(translate::monitors_to_info(
                &api::monitors().map_err(api_error)?,
            ))
        }

        /// A window destroyed per the hook: report removal when we knew
        /// it, nothing otherwise (the environment ignores removals of
        /// windows it never had).
        fn apply_removal(&mut self, hwnd: u64) -> Option<EnvironmentEvent> {
            if self.windows.remove(&hwnd).is_some() {
                Some(EnvironmentEvent::WindowRemoved { id: hwnd })
            } else {
                None
            }
        }

        /// Re-query one window and emit the right event: added,
        /// changed, or (when it no longer passes the filter, e.g. it
        /// was hidden or cloaked) removed.
        fn apply_refresh(&mut self, hwnd: u64) -> Option<EnvironmentEvent> {
            let raw = api::query_window(hwnd, &mut self.exe_cache).ok()??;
            let info = translate::window_to_info(&raw)?;
            let known = self.windows.contains_key(&info.id);
            self.windows.insert(info.id, info.clone());
            Some(if known {
                EnvironmentEvent::WindowChanged(info)
            } else {
                EnvironmentEvent::WindowAdded(info)
            })
        }
    }

    impl Drop for State {
        fn drop(&mut self) {
            // Hook thread teardown: unhook, kill timer, join. A lost
            // thread (panic) logs but must not panic the dropper.
            if let Some(hook) = self.hook.take() {
                hook.stop();
                tracing::debug!("win32 event hook stopped");
            }
        }
    }
}
