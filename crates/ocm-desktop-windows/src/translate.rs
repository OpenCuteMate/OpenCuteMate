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

use ocm_desktop::{OutputInfo, OutputPhysical, OutputTransform};

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
