//! Pure translation layer between raw platform observations and the
//! neutral [`ocm_desktop`] model.
//!
//! Everything here works on **plain data** (tuples, ids, flag booleans)
//! so it compiles and is unit-tested on every OS — including the
//! fixtures captured from real Windows API shapes. Only the `api`
//! module (cfg(windows)) touches Win32; it fills these raw structs and
//! the translation is identical everywhere.
//!
//! # Coordinate conventions (Phase 3c)
//!
//! The neutral model's global space is **logical pixels** (the Phase 3b
//! convention, matching Wayland compositors). Windows' native global
//! space is physical pixels of the virtual screen; this layer converts:
//!
//! - monitor and window rectangles: physical ÷ scale (exact `f64`,
//!   never rounded mid-conversion);
//! - scale = effective DPI ÷ 96 (per monitor; mixed-DPI arrangements
//!   are fully supported — one monitor at 150% next to one at 100% is
//!   the tested case);
//! - work areas are rounded to whole logical px when they land on
//!   fractional positions (a sub-pixel approximation, ≤ 1 px, inherent
//!   to the integer `Rect` model; documented limitation).
//!
//! A window straddling two monitors with different scales is converted
//! with the scale of the monitor containing the majority of it (what
//! `MonitorFromWindow` reports) — the same approximation Wayland's
//! mixed-DPI compositors make for cross-output windows.

// The pure translation layer exists for the cfg(windows) API layer;
// on other platforms only its tests consume it.
#![cfg_attr(not(windows), allow(dead_code))]

use ocm_desktop::{
    OutputInfo, OutputPhysical, OutputTransform, WindowGeometry, WindowInfo, WindowState,
};

/// Windows' `DEVMODE.dmDisplayOrientation` values (physical constants,
/// mirrored here so the translation is testable on every OS).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Orientation {
    /// `DMDO_DEFAULT`
    Default,
    /// `DMDO_90` — Windows counts rotations clockwise.
    Cw90,
    /// `DMDO_180`
    Cw180,
    /// `DMDO_270`
    Cw270,
}

/// A monitor as observed through the Win32 monitor APIs: physical
/// pixels in the global virtual-screen coordinate space, filled by the
/// cfg(windows) API layer.
#[derive(Debug, Clone)]
pub struct RawMonitor {
    /// Device name (`GetMonitorInfoW.szDevice`, e.g. `\\.\DISPLAY1`).
    /// Stable for the session; used as the output id (the same role the
    /// Wayland output name plays on Niri).
    pub device: String,
    /// `rcMonitor` — full monitor rect (physical: left, top, right,
    /// bottom). May be negative for monitors left of / above the
    /// primary.
    pub full: (i32, i32, i32, i32),
    /// `rcWork` — the work area (monitor minus taskbar and other
    /// reserved/appbar strips), physical.
    pub work: (i32, i32, i32, i32),
    /// Effective DPI (`GetDpiForMonitor`, MDT_EFFECTIVE_DPI). 96 = 100%.
    pub dpi: u32,
    /// `MONITORINFOF_PRIMARY`.
    pub primary: bool,
    pub orientation: Orientation,
    /// `dmPelsWidth`/`dmPelsHeight` — the current mode's physical
    /// resolution.
    pub resolution: (u32, u32),
    /// `dmDisplayFrequency` (Hz); `None` when the driver reports 0.
    pub refresh_hz: Option<u32>,
}

/// Windows reports rotations clockwise; the neutral transform uses the
/// Wayland convention (counter-clockwise), so the directions invert.
/// The direction convention itself needs validation on rotated hardware
/// (capability matrix: "implemented, direction pending validation").
pub fn transform_from_orientation(orientation: Orientation) -> OutputTransform {
    match orientation {
        Orientation::Default => OutputTransform::Normal,
        Orientation::Cw90 => OutputTransform::Rotate270,
        Orientation::Cw180 => OutputTransform::Rotate180,
        Orientation::Cw270 => OutputTransform::Rotate90,
    }
}

/// Effective scale factor from a monitor DPI (96 = 100%). Degenerate
/// DPI (0, e.g. a driver that failed the query) falls back to 100%.
pub fn scale_from_dpi(dpi: u32) -> f64 {
    if dpi == 0 {
        1.0
    } else {
        dpi as f64 / 96.0
    }
}

/// Physical (x, y) → global logical, exact division by the scale.
pub fn physical_point_to_logical(point: (i32, i32), scale: f64) -> (f64, f64) {
    (point.0 as f64 / scale, point.1 as f64 / scale)
}

/// A physical (left, top, right, bottom) rect → global logical
/// (x, y, width, height), exact.
pub fn physical_rect_to_logical(rect: (i32, i32, i32, i32), scale: f64) -> (f64, f64, f64, f64) {
    (
        rect.0 as f64 / scale,
        rect.1 as f64 / scale,
        (rect.2 - rect.0) as f64 / scale,
        (rect.3 - rect.1) as f64 / scale,
    )
}

/// Raw monitor → neutral [`OutputInfo`].
///
/// `work_area` is output-local logical, rounded to whole px (documented
/// sub-pixel approximation for taskbar strips that land on fractional
/// logical positions).
pub fn monitor_to_info(monitor: &RawMonitor) -> OutputInfo {
    let scale = scale_from_dpi(monitor.dpi);
    let (lx, ly) = physical_point_to_logical((monitor.full.0, monitor.full.1), scale);
    let (_, _, lw, lh) = physical_rect_to_logical(monitor.full, scale);
    // Work area is output-local: physical offsets from the monitor's
    // own origin, converted exactly, rounded only for the integer Rect.
    let work_offset = (
        (monitor.work.0 - monitor.full.0) as f64 / scale,
        (monitor.work.1 - monitor.full.1) as f64 / scale,
    );
    let (_, _, ww, wh) = physical_rect_to_logical(monitor.work, scale);
    OutputInfo {
        id: monitor.device.clone(),
        name: monitor.device.clone(),
        logical_position: Some((lx as i32, ly as i32)),
        logical_size: Some((lw.max(1.0) as u32, lh.max(1.0) as u32)),
        scale: Some(scale),
        transform: Some(transform_from_orientation(monitor.orientation)),
        physical: Some(OutputPhysical {
            resolution: monitor.resolution,
            refresh_mhz: monitor.refresh_hz.map(|hz| hz * 1000),
        }),
        work_area: Some(ocm_desktop::Rect {
            x: work_offset.0.round() as i32,
            y: work_offset.1.round() as i32,
            width: ww.round().max(1.0) as i32,
            height: wh.round().max(1.0) as i32,
        }),
        primary: Some(monitor.primary),
    }
}

/// All raw monitors → neutral outputs, sorted by id for a stable order.
pub fn monitors_to_info(monitors: &[RawMonitor]) -> Vec<OutputInfo> {
    let mut outputs: Vec<OutputInfo> = monitors.iter().map(monitor_to_info).collect();
    outputs.sort_by(|a, b| a.id.cmp(&b.id));
    outputs
}

// ---------------------------------------------------------------------------
// Windows
// ---------------------------------------------------------------------------

/// A toplevel window as observed through the Win32 window APIs —
/// physical pixels, filled by the cfg(windows) API layer.
///
/// `frame` is the **visible window rectangle**: DWM extended frame
/// bounds when available (excludes the invisible resize borders
/// `GetWindowRect` includes), falling back to `GetWindowRect`. This is
/// the rectangle the neutral `WindowInfo.geometry` represents, and it
/// is the same thing the user perceives as "the window".
#[derive(Debug, Clone)]
pub struct RawWindow {
    /// HWND as an opaque id (stable for the window's lifetime).
    pub hwnd: u64,
    /// Title text (`GetWindowTextW`), empty when the window has none.
    pub title: String,
    /// Executable file stem, lowercased (`firefox`, `explorer`), when
    /// the process could be opened (privileged processes cannot).
    pub exe: Option<String>,
    pub pid: Option<u32>,
    /// DWM extended frame bounds / `GetWindowRect` fallback (physical:
    /// left, top, right, bottom in the virtual-screen space).
    pub frame: (i32, i32, i32, i32),
    /// `IsWindowVisible`.
    pub visible: bool,
    /// `IsIconic`.
    pub minimized: bool,
    /// `IsZoomed`.
    pub maximized: bool,
    /// Equal to the foreground window at enumeration time.
    pub foreground: bool,
    /// DWMWA_CLOAKED != 0 (e.g. suspended UWP apps: alive, not on
    /// screen).
    pub cloaked: bool,
    /// `WS_EX_TOOLWINDOW` (palettes, toolbars — not task-switchable).
    pub tool_window: bool,
    /// Effective DPI of the monitor the window is on (used for the
    /// physical→logical conversion; `MonitorFromWindow` +
    /// `GetDpiForMonitor`).
    pub monitor_dpi: u32,
}

/// Window relevance filter (pure, fixture-testable).
///
/// Excluded — and why:
/// - **invisible**: not on screen at all (`IsWindowVisible` covers the
///   parent chain);
/// - **cloaked**: suspended UWP apps that are "alive" per Win32 but not
///   rendered (`DWMWA_CLOAKED`);
/// - **tool windows** (`WS_EX_TOOLWINDOW`): palettes and toolbars that
///   the shell itself excludes from the task switcher.
///
/// Everything else `EnumWindows` reports is kept — owned dialogs,
/// console windows, browser popups included — so legitimate
/// applications are never over-filtered.
pub fn is_relevant_window(raw: &RawWindow) -> bool {
    raw.visible && !raw.cloaked && !raw.tool_window
}

/// Raw window → neutral [`WindowInfo`], or `None` when filtered out or
/// when there is nothing honest to report.
///
/// Geometry conversion uses the window's monitor DPI (physical ÷
/// scale, exact). Minimized windows have no on-screen position
/// (`GetWindowRect` reports the -32000 parking spot), so their
/// geometry is `None` — absence, not a guess.
pub fn window_to_info(raw: &RawWindow) -> Option<WindowInfo> {
    if !is_relevant_window(raw) {
        return None;
    }
    let scale = scale_from_dpi(raw.monitor_dpi);
    let geometry = if raw.minimized {
        None
    } else {
        let (x, y, width, height) = physical_rect_to_logical(raw.frame, scale);
        Some(WindowGeometry {
            x,
            y,
            width,
            height,
        })
    };
    Some(WindowInfo {
        id: raw.hwnd,
        title: (!raw.title.is_empty()).then_some(raw.title.clone()),
        app_id: raw.exe.clone(),
        pid: raw.pid,
        workspace_id: None,
        state: WindowState {
            focused: Some(raw.foreground),
            minimized: Some(raw.minimized),
            maximized: Some(raw.maximized),
            // Rect-vs-monitor comparison would be a heuristic: refused.
            fullscreen: None,
            // Windows has no tiling/floating concept to report.
            floating: None,
            visible: Some(raw.visible),
            // Deliberate no: Windows' "urgent" is a flash request we do
            // not subscribe to, not a state we can query honestly.
            urgent: None,
        },
        geometry,
    })
}

// ---------------------------------------------------------------------------
// WinEvent translation
// ---------------------------------------------------------------------------

// `SetWinEventHook` event constants (stable, documented Win32 values,
// mirrored so the mapping is fixture-testable on every OS).
pub mod win_events {
    pub const EVENT_SYSTEM_FOREGROUND: u32 = 0x0003;
    pub const EVENT_SYSTEM_MINIMIZESTART: u32 = 0x0016;
    pub const EVENT_SYSTEM_MINIMIZEEND: u32 = 0x0017;
    pub const EVENT_OBJECT_CREATE: u32 = 0x8000;
    pub const EVENT_OBJECT_DESTROY: u32 = 0x8001;
    pub const EVENT_OBJECT_SHOW: u32 = 0x8002;
    pub const EVENT_OBJECT_HIDE: u32 = 0x8003;
    pub const EVENT_OBJECT_STATECHANGE: u32 = 0x800A;
    pub const EVENT_OBJECT_LOCATIONCHANGE: u32 = 0x800B;
    /// `OBJID_WINDOW` (idObject value identifying the window itself).
    pub const OBJID_WINDOW: i32 = 0;
}

/// Sentinel event code sent by the provider's own pump (never a real
/// WinEvent): the monitor re-enumeration tick.
pub const PUMP_MONITOR_REFRESH: u32 = u32::MAX;

/// One raw WinEvent as delivered by the hook callback.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RawWinEvent {
    pub code: u32,
    pub hwnd: u64,
    /// `idObject` from the callback (only `OBJID_WINDOW` events are
    /// forwarded by the API layer).
    pub object: i32,
}

/// What the provider must do for one raw WinEvent. The expensive work
/// (re-querying a window) happens once per window per poll batch, not
/// per event.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum EventAction {
    /// Known but irrelevant (system events, child-object events,
    /// future event codes).
    Ignore,
    /// Re-query this window: it may have appeared, moved, resized,
    /// changed state or become filtered.
    RefreshWindow,
    /// The window is gone.
    WindowRemoved,
    /// The window gained foreground focus.
    Focused,
    /// Re-enumerate monitors (monitor configuration may have changed).
    MonitorRefresh,
}

/// Map one raw WinEvent to an action.
///
/// Coverage: create/destroy/show/hide/location/state/minimize events
/// and foreground focus. Deliberately ignored: `EVENT_OBJECT_REORDER`
/// (z-order is not modeled), `EVENT_OBJECT_FOCUS` per-element focus
/// noise (the coarser `EVENT_SYSTEM_FOREGROUND` is the focus source),
/// and anything unknown (forward compatibility).
pub fn map_win_event(event: &RawWinEvent) -> EventAction {
    use win_events::*;
    // The provider pump's own sentinel (never a real WinEvent) is not
    // tied to a window.
    if event.code == PUMP_MONITOR_REFRESH {
        return EventAction::MonitorRefresh;
    }
    if event.object != OBJID_WINDOW {
        return EventAction::Ignore;
    }
    if event.hwnd == 0 {
        return EventAction::Ignore;
    }
    match event.code {
        EVENT_OBJECT_CREATE
        | EVENT_OBJECT_SHOW
        | EVENT_OBJECT_HIDE
        | EVENT_OBJECT_LOCATIONCHANGE
        | EVENT_OBJECT_STATECHANGE
        | EVENT_SYSTEM_MINIMIZESTART
        | EVENT_SYSTEM_MINIMIZEEND => EventAction::RefreshWindow,
        EVENT_OBJECT_DESTROY => EventAction::WindowRemoved,
        EVENT_SYSTEM_FOREGROUND => EventAction::Focused,
        _ => EventAction::Ignore,
    }
}
