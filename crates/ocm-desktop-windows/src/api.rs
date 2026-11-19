//! The cfg(windows) API layer: Win32 calls that fill the pure
//! translation layer's raw structs.
//!
//! Every function that reads geometry runs under a per-thread
//! `DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2` context so the Win32
//! rects are true physical pixels regardless of the process's DPI
//! awareness (virtualization would otherwise lie about coordinates on
//! scaled monitors). Contexts are set and restored around each call so
//! embedding this provider into a larger process never changes that
//! process's DPI behavior.
//!
//! Callbacks handed to Win32 (`EnumDisplayMonitors`, `EnumWindows`,
//! `WINEVENTPROC`) are panic-free by construction: no unwraps, no
//! indexing — a panic unwinding into a system callback is undefined
//! behavior.

#![cfg(windows)]

use std::mem;

use windows::core::{BOOL, PCWSTR};
use windows::Win32::Foundation::{LPARAM, RECT};
use windows::Win32::Graphics::Gdi::{
    EnumDisplayMonitors, EnumDisplaySettingsW, GetMonitorInfoW, DEVMODEW, HDC, HMONITOR,
    MONITORINFOEXW,
};
use windows::Win32::UI::HiDpi::{
    GetDpiForMonitor, SetThreadDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
    MDT_EFFECTIVE_DPI,
};

use crate::translate::{Orientation, RawMonitor};

/// Errors of the Win32 API layer.
#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    #[error("win32 call failed: {0}")]
    Call(String),
}

/// Run `f` with per-monitor-v2 DPI awareness pinned on this thread,
/// restoring the previous context afterwards.
pub(crate) fn with_thread_dpi<T>(f: impl FnOnce() -> T) -> T {
    unsafe {
        let previous = SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        let out = f();
        SetThreadDpiAwarenessContext(previous);
        out
    }
}

/// Enumerate the current monitors (physical-pixel coordinates resolved
/// under per-monitor-v2 awareness).
pub(crate) fn monitors() -> Result<Vec<RawMonitor>, ApiError> {
    with_thread_dpi(|| {
        let mut found: Vec<RawMonitor> = Vec::new();
        let data = LPARAM(&mut found as *mut Vec<RawMonitor> as isize);
        // The callback only appends through the raw pointer; Win32 calls
        // it synchronously and re-entrantly-free.
        let ok = unsafe { EnumDisplayMonitors(None, None, Some(monitor_proc), data) };
        if !ok.as_bool() {
            return Err(ApiError::Call("EnumDisplayMonitors failed".into()));
        }
        Ok(found)
    })
}

unsafe extern "system" fn monitor_proc(
    hmonitor: HMONITOR,
    _hdc: HDC,
    _rect: *mut RECT,
    lparam: LPARAM,
) -> BOOL {
    // No unwraps here: this runs inside a system callback.
    let results = &mut *(lparam.0 as *mut Vec<RawMonitor>);
    let mut info = MONITORINFOEXW::default();
    info.monitorInfo.cbSize = mem::size_of::<MONITORINFOEXW>() as u32;
    if !GetMonitorInfoW(hmonitor, &mut info as *mut _ as *mut _).as_bool() {
        // A monitor that stopped reporting between enumeration and the
        // info query: skip it instead of inventing geometry.
        return true.into();
    }

    let full = rect_tuple(info.monitorInfo.rcMonitor);
    let work = rect_tuple(info.monitorInfo.rcWork);
    let primary = (info.monitorInfo.dwFlags
        & windows::Win32::UI::WindowsAndMessaging::MONITORINFOF_PRIMARY)
        != 0;

    // Per-monitor effective DPI; a failed query (old driver) means 100%.
    let mut dpi_x: u32 = 0;
    let mut dpi_y: u32 = 0;
    let dpi = if GetDpiForMonitor(hmonitor, MDT_EFFECTIVE_DPI, &mut dpi_x, &mut dpi_y).is_ok()
        && dpi_x > 0
    {
        dpi_x
    } else {
        96
    };

    // Current mode: physical resolution, refresh rate and orientation
    // (queried through the device name the monitor info carries).
    let device: Vec<u16> = info
        .szDevice
        .iter()
        .copied()
        .take_while(|c| *c != 0)
        .collect();
    let mut orientation = Orientation::Default;
    let mut refresh_hz = None;
    let mut resolution = {
        // Fall back to the monitor rect's physical size when the mode
        // query fails; that is the same physical extent.
        let (x, y, w, h) = rect_tuple(info.monitorInfo.rcMonitor);
        ((w - x).max(0) as u32, (h - y).max(0) as u32)
    };
    if !device.is_empty() {
        let mut mode = DEVMODEW::default();
        mode.dmSize = mem::size_of::<DEVMODEW>() as u16;
        if EnumDisplaySettingsW(
            PCWSTR::from_raw(device.as_ptr()),
            windows::Win32::Graphics::Gdi::ENUM_CURRENT_SETTINGS,
            &mut mode,
        )
        .as_bool()
        {
            orientation = match mode.Anonymous1.Anonymous2.dmDisplayOrientation.0 {
                1 => Orientation::Cw90,
                2 => Orientation::Cw180,
                3 => Orientation::Cw270,
                _ => Orientation::Default,
            };
            refresh_hz = (mode.dmDisplayFrequency > 0).then_some(mode.dmDisplayFrequency);
            resolution = (mode.dmPelsWidth, mode.dmPelsHeight);
        }
    }

    results.push(RawMonitor {
        device: String::from_utf16_lossy(&device),
        full,
        work,
        dpi,
        primary,
        orientation,
        resolution,
        refresh_hz,
    });
    true.into()
}

fn rect_tuple(rect: RECT) -> (i32, i32, i32, i32) {
    (rect.left, rect.top, rect.right, rect.bottom)
}
