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

use std::collections::BTreeMap;
use std::mem;

use windows::core::{BOOL, PCWSTR, PWSTR};
use windows::Win32::Foundation::{CloseHandle, HWND, LPARAM, RECT};
use windows::Win32::Graphics::Dwm::{
    DwmGetWindowAttribute, DWMWA_CLOAKED, DWMWA_EXTENDED_FRAME_BOUNDS,
};
use windows::Win32::Graphics::Gdi::{
    EnumDisplayMonitors, EnumDisplaySettingsW, GetMonitorInfoW, MonitorFromWindow, DEVMODEW, HDC,
    HMONITOR, MONITORINFOEXW, MONITOR_DEFAULTTONEAREST,
};
use windows::Win32::System::Threading::{
    OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::UI::HiDpi::{
    GetDpiForMonitor, SetThreadDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
    MDT_EFFECTIVE_DPI,
};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetForegroundWindow, GetWindowLongPtrW, GetWindowRect, GetWindowTextLengthW,
    GetWindowTextW, GetWindowThreadProcessId, IsIconic, IsWindowVisible, IsZoomed, GWL_EXSTYLE,
    WS_EX_TOOLWINDOW,
};

use crate::translate::{Orientation, RawMonitor, RawWindow};

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

/// Per-pid executable-name cache (`QueryFullProcessImageNameW` is not
/// free and process images do not change): passed in by the provider so
/// repeated enumerations stay cheap.
pub(crate) type ExeCache = BTreeMap<u32, Option<String>>;

/// Enumerate the visible, non-cloaked, non-tool toplevel windows
/// (z-order top to bottom, as `EnumWindows` walks them), each with the
/// real visible frame bounds, per-monitor DPI conversion inputs and
/// state flags. Returns raw records; translation happens in
/// `translate::window_to_info`.
///
/// `exe_cache` persists across calls so process names are queried once.
pub(crate) fn enumerate_windows(exe_cache: &mut ExeCache) -> Result<Vec<RawWindow>, ApiError> {
    with_thread_dpi(|| {
        let foreground = unsafe { GetForegroundWindow() };
        let mut found: Vec<RawWindow> = Vec::new();
        let data = WindowEnumData {
            out: &mut found,
            exe_cache,
            foreground,
        };
        let ok = unsafe {
            EnumWindows(
                Some(enum_window_proc),
                LPARAM(&data as *const WindowEnumData as isize),
            )
        };
        if !ok.is_ok() {
            return Err(ApiError::Call("EnumWindows failed".into()));
        }
        Ok(found)
    })
}

struct WindowEnumData<'a> {
    out: &'a mut Vec<RawWindow>,
    exe_cache: &'a mut ExeCache,
    foreground: HWND,
}

unsafe extern "system" fn enum_window_proc(hwnd: HWND, lparam: LPARAM) -> BOOL {
    // No unwraps here: this runs inside a system callback. Any window
    // that fails a query is skipped, never defaulted.
    let data = &mut *(lparam.0 as *mut WindowEnumData);

    if !IsWindowVisible(hwnd).as_bool() {
        return true.into();
    }
    // Suspended UWP apps are alive but not rendered.
    if cloaked_state(hwnd) != 0 {
        return true.into();
    }
    // Palettes/toolbars the shell itself excludes from task switching.
    let ex_style = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
    if (ex_style & WS_EX_TOOLWINDOW.0 as isize) != 0 {
        return true.into();
    }

    let title = window_text(hwnd, GetWindowTextLengthW(hwnd));
    let frame = extended_frame_bounds(hwnd).unwrap_or_else(|| window_rect(hwnd));
    // A monitor that cannot report DPI still yields an honest record at
    // 100%; the conversion layer documents this fallback.
    let monitor_dpi = monitor_dpi(hwnd);
    let minimized = IsIconic(hwnd).as_bool();
    let maximized = IsZoomed(hwnd).as_bool();

    let mut pid: u32 = 0;
    GetWindowThreadProcessId(hwnd, Some(&mut pid));
    let exe = if pid != 0 {
        data.exe_cache
            .entry(pid)
            .or_insert_with(|| process_image(pid))
            .clone()
    } else {
        None
    };

    data.out.push(RawWindow {
        hwnd: hwnd.0 as u64,
        title,
        exe,
        pid: (pid != 0).then_some(pid),
        frame,
        visible: true,
        minimized,
        maximized,
        foreground: hwnd == data.foreground,
        cloaked: false,
        tool_window: false,
        monitor_dpi,
    });
    true.into()
}

/// DWMWA_CLOAKED != 0 (0 when DWM or the attribute are unavailable).
fn cloaked_state(hwnd: HWND) -> u32 {
    let mut cloaked: u32 = 0;
    let _ = unsafe {
        DwmGetWindowAttribute(
            hwnd,
            DWMWA_CLOAKED,
            (&mut cloaked as *mut u32).cast(),
            mem::size_of::<u32>() as u32,
        )
    };
    cloaked
}

/// The visible window bounds (DWM extended frame bounds: excludes the
/// invisible resize borders `GetWindowRect` includes).
fn extended_frame_bounds(hwnd: HWND) -> Option<(i32, i32, i32, i32)> {
    let mut rect = RECT::default();
    let ok = unsafe {
        DwmGetWindowAttribute(
            hwnd,
            DWMWA_EXTENDED_FRAME_BOUNDS,
            (&mut rect as *mut RECT).cast(),
            mem::size_of::<RECT>() as u32,
        )
    };
    ok.ok().map(|()| rect_tuple(rect))
}

fn window_rect(hwnd: HWND) -> (i32, i32, i32, i32) {
    let mut rect = RECT::default();
    // Failure leaves the default (all-zero) rect; callers downstream
    // filter degenerate geometry out.
    unsafe { GetWindowRect(hwnd, &mut rect) };
    rect_tuple(rect)
}

fn monitor_dpi(hwnd: HWND) -> u32 {
    let hmonitor = unsafe { MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST) };
    let mut dpi_x: u32 = 0;
    let mut dpi_y: u32 = 0;
    if unsafe { GetDpiForMonitor(hmonitor, MDT_EFFECTIVE_DPI, &mut dpi_x, &mut dpi_y) }.is_ok()
        && dpi_x > 0
    {
        dpi_x
    } else {
        96
    }
}

/// Title text; windows without a title yield an empty string.
fn window_text(hwnd: HWND, len: i32) -> String {
    if len <= 0 {
        return String::new();
    }
    let mut buf = vec![0u16; len as usize + 1];
    let written = unsafe { GetWindowTextW(hwnd, &mut buf) };
    String::from_utf16_lossy(&buf[..written.max(0) as usize])
}

/// Executable file stem, lowercased; `None` when the process cannot be
/// opened (elevated processes are not readable by a normal client).
fn process_image(pid: u32) -> Option<String> {
    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }.ok()?;
    let mut name = [0u16; 1024];
    let mut size: u32 = name.len() as u32;
    let ok = unsafe {
        QueryFullProcessImageNameW(
            handle,
            PROCESS_NAME_WIN32,
            PWSTR(name.as_mut_ptr()),
            &mut size,
        )
    };
    let _ = unsafe { CloseHandle(handle) };
    if !ok.is_ok() {
        return None;
    }
    let path = String::from_utf16_lossy(&name[..size as usize]);
    let stem = path.rsplit(['\\', '/']).next().unwrap_or(&path);
    let stem = stem.strip_suffix(".exe").unwrap_or(stem);
    Some(stem.to_ascii_lowercase())
}
