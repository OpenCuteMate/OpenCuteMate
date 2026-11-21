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
use windows::Win32::Foundation::{CloseHandle, HWND, LPARAM, RECT, WPARAM};
use windows::Win32::Graphics::Dwm::{
    DwmGetWindowAttribute, DWMWA_CLOAKED, DWMWA_EXTENDED_FRAME_BOUNDS,
};
use windows::Win32::Graphics::Gdi::{
    EnumDisplayMonitors, EnumDisplaySettingsW, GetMonitorInfoW, MonitorFromWindow, DEVMODEW, HDC,
    HMONITOR, MONITORINFOEXW, MONITOR_DEFAULTTONEAREST,
};
use windows::Win32::System::Threading::{
    GetCurrentThreadId, OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32,
    PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::UI::HiDpi::{
    GetDpiForMonitor, SetThreadDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
    MDT_EFFECTIVE_DPI,
};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetForegroundWindow, GetWindowLongPtrW, GetWindowRect, GetWindowTextLengthW,
    GetWindowTextW, GetWindowThreadProcessId, IsIconic, IsWindow, IsWindowVisible, IsZoomed,
    KillTimer, PostThreadMessageW, SetTimer, GWL_EXSTYLE, WM_QUIT, WM_TIMER, WS_EX_TOOLWINDOW,
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
        let mut mode = DEVMODEW {
            dmSize: mem::size_of::<DEVMODEW>() as u16,
            ..DEVMODEW::default()
        };
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
    if let Some(raw) = collect_window(hwnd, data.exe_cache, data.foreground) {
        data.out.push(raw);
    }
    true.into()
}

/// Query one window (used by the enumerator and by event-triggered
/// refreshes). Returns `None` for windows that fail `IsWindow`, are
/// invisible, cloaked or tool windows — the same filter as
/// enumeration, so event refreshes and snapshots agree.
unsafe fn collect_window(
    hwnd: HWND,
    exe_cache: &mut ExeCache,
    foreground: HWND,
) -> Option<RawWindow> {
    if !IsWindow(Some(hwnd)).as_bool() || !IsWindowVisible(hwnd).as_bool() {
        return None;
    }
    // Suspended UWP apps are alive but not rendered.
    if cloaked_state(hwnd) != 0 {
        return None;
    }
    // Palettes/toolbars the shell itself excludes from task switching.
    let ex_style = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
    if (ex_style & WS_EX_TOOLWINDOW.0 as isize) != 0 {
        return None;
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
        exe_cache
            .entry(pid)
            .or_insert_with(|| process_image(pid))
            .clone()
    } else {
        None
    };

    Some(RawWindow {
        hwnd: hwnd.0 as u64,
        title,
        exe,
        pid: (pid != 0).then_some(pid),
        frame,
        visible: true,
        minimized,
        maximized,
        foreground: hwnd == foreground,
        cloaked: false,
        tool_window: false,
        monitor_dpi,
    })
}

/// Query a single window for an event-triggered refresh. `exe_cache`
/// must be the provider's cache so names stay consistent.
pub(crate) fn query_window(
    hwnd: u64,
    exe_cache: &mut ExeCache,
) -> Result<Option<RawWindow>, ApiError> {
    with_thread_dpi(|| {
        let foreground = unsafe { GetForegroundWindow() };
        Ok(unsafe { collect_window(HWND(hwnd as *mut core::ffi::c_void), exe_cache, foreground) })
    })
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
    let _ = unsafe { GetWindowRect(hwnd, &mut rect) };
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

// ---------------------------------------------------------------------------
// Event tracking: hook thread + message pump
// ---------------------------------------------------------------------------

/// One raw hook event forwarded to the provider.
pub(crate) type RawEvent = crate::translate::RawWinEvent;

/// A wake callback the provider sets and the hook thread invokes after
/// enqueueing events (the consumer's loop waits on an OS handle).
pub(crate) type WakeFn = std::sync::Arc<dyn Fn() + Send + Sync>;

/// Shared wake slot (provider side sets it; the pump reads it).
#[derive(Clone, Default)]
pub(crate) struct WakeSlot(std::sync::Arc<std::sync::Mutex<Option<WakeFn>>>);

impl WakeSlot {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set(&self, wake: WakeFn) {
        if let Ok(mut slot) = self.0.lock() {
            *slot = Some(wake);
        }
    }

    /// Invoke the callback without holding the lock (the callback may
    /// signal an OS object; it must never re-enter this slot).
    fn notify(&self) {
        let wake = self.0.lock().ok().and_then(|slot| slot.clone());
        if let Some(wake) = wake {
            wake();
        }
    }
}

/// Handle to the running hook thread.
pub(crate) struct HookHandle {
    /// The pump thread's id, so teardown can post `WM_QUIT` to it.
    thread_id: u32,
    /// Join handle for a clean shutdown.
    join: std::thread::JoinHandle<()>,
}

impl HookHandle {
    /// Ask the pump thread to exit (unhook + kill timer + join).
    pub fn stop(self) {
        unsafe {
            let _ = PostThreadMessageW(self.thread_id, WM_QUIT, WPARAM(0), LPARAM(0));
        }
        let _ = self.join.join();
    }
}

/// Start the event-hook thread.
///
/// Win32 delivers `SetWinEventHook` callbacks on the thread that
/// installed the hook, and only while that thread pumps messages — so
/// the hook lives on a dedicated thread running a `GetMessageW` pump.
/// The same pump receives `WM_TIMER` (the monitor re-enumeration
/// tick), so no extra polling thread exists.
///
/// Events are forwarded over a bounded channel; the callback drops
/// (and counts, logged once) events when a slow consumer lets it fill.
/// Returns only after the pump thread reports its id and a successfully
/// installed hook — a hook failure surfaces as an error from connect.
///
/// `monitor_tick_ms` is the monitor re-enumeration interval.
pub(crate) fn start_event_thread(
    sender: std::sync::mpsc::SyncSender<RawEvent>,
    monitor_tick_ms: u32,
    wake: WakeSlot,
) -> Result<HookHandle, ApiError> {
    // The pump reports its thread id (and the hook outcome) over a
    // one-shot channel; a timeout bounds a stuck thread.
    let (id_tx, id_rx) = std::sync::mpsc::channel::<Result<u32, String>>();
    let join = std::thread::Builder::new()
        .name("ocm-desktop-win-events".into())
        .spawn(move || unsafe { pump(sender, monitor_tick_ms, id_tx, wake) })
        .map_err(|e| ApiError::Call(format!("event thread spawn: {e}")))?;

    match id_rx.recv_timeout(std::time::Duration::from_secs(5)) {
        Ok(Ok(thread_id)) => Ok(HookHandle { thread_id, join }),
        Ok(Err(reason)) => {
            let _ = join.join();
            Err(ApiError::Call(reason))
        }
        Err(e) => {
            let _ = join.join();
            Err(ApiError::Call(format!("event thread startup: {e}")))
        }
    }
}

thread_local! {
    /// The pump thread's own sink. Win32 delivers the hook's callbacks
    /// on this thread only, so a thread-local is the correct (and
    /// lock-free) way for the callback to reach it.
    static SENDER: std::cell::RefCell<Option<std::sync::mpsc::SyncSender<RawEvent>>> =
        const { std::cell::RefCell::new(None) };
    /// Wake slot for the pump thread (callbacks run on this thread).
    static WAKE: std::cell::RefCell<Option<WakeSlot>> = const { std::cell::RefCell::new(None) };
}

/// Total events dropped when the bounded channel was full.
static DROPPED: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

unsafe extern "system" fn win_event_proc(
    _hook: windows::Win32::UI::Accessibility::HWINEVENTHOOK,
    event: u32,
    hwnd: HWND,
    id_object: i32,
    _id_child: i32,
    _thread: u32,
    _time: u32,
) {
    // Callback contract: never panic, never block for long. Filtering
    // to OBJID_WINDOW happens here — that is the highest-volume noise:
    // child objects of every control in every app.
    if id_object != crate::translate::win_events::OBJID_WINDOW || hwnd.0.is_null() {
        return;
    }
    let raw = RawEvent {
        code: event,
        hwnd: hwnd.0 as u64,
        object: id_object,
    };
    let sent = SENDER.with(|sink| sink.borrow().as_ref().map(|s| s.try_send(raw).is_ok()));
    if sent == Some(true) {
        WAKE.with(|wake| {
            if let Some(wake) = wake.borrow().as_ref() {
                wake.notify();
            }
        });
    } else if sent == Some(false) {
        let dropped = DROPPED.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        if dropped == 0 {
            tracing::warn!("event channel full; dropping desktop events until it drains");
        }
    }
}

unsafe fn pump(
    sender: std::sync::mpsc::SyncSender<RawEvent>,
    monitor_tick_ms: u32,
    ready: std::sync::mpsc::Sender<Result<u32, String>>,
    wake: WakeSlot,
) {
    use windows::Win32::UI::Accessibility::{SetWinEventHook, UnhookWinEvent};
    use windows::Win32::UI::WindowsAndMessaging::{
        DispatchMessageW, GetMessageW, TranslateMessage, MSG, WINEVENT_OUTOFCONTEXT,
    };

    // The thread-local sink and wake slot must exist before the hook can
    // fire. The wake notify closure runs on this same thread; the
    // Mutex+Arc keeps the slot accessible from the pump's timer branch.
    SENDER.with(|sink| *sink.borrow_mut() = Some(sender));
    WAKE.with(|slot| *slot.borrow_mut() = Some(wake.clone()));
    let thread_id = GetCurrentThreadId();

    // One hook over both event families we consume (system 0x0003.. and
    // object 0x8000..): unknown codes are mapped to Ignore downstream.
    let hook = SetWinEventHook(
        crate::translate::win_events::EVENT_SYSTEM_FOREGROUND,
        crate::translate::win_events::EVENT_OBJECT_LOCATIONCHANGE,
        None,
        Some(win_event_proc),
        0,
        thread_id,
        WINEVENT_OUTOFCONTEXT,
    );
    if hook.is_invalid() {
        // Close the sink first so the provider's next poll sees the
        // loss, then report the failure to the starter.
        SENDER.with(|sink| sink.borrow_mut().take());
        let _ = ready.send(Err("SetWinEventHook failed".into()));
        return;
    }
    if ready.send(Ok(thread_id)).is_err() {
        // The starter gave up (or was dropped): tear down immediately.
        let _ = UnhookWinEvent(hook);
        return;
    }
    let _ = ready;

    // The monitor re-enumeration tick rides the same pump.
    let timer = SetTimer(None, 0, monitor_tick_ms, None);

    let mut msg = MSG::default();
    loop {
        // 0 = WM_QUIT, -1 = pump error; anything else is a message.
        let r = GetMessageW(&mut msg, None, 0, 0);
        if r.0 == 0 || r.0 == -1 {
            if r.0 == -1 {
                tracing::error!("event pump error; Windows event tracking stopped");
            }
            break;
        }
        if msg.message == WM_TIMER {
            let sentinel = RawEvent {
                // Cannot collide with a real WinEvent.
                code: crate::translate::PUMP_MONITOR_REFRESH,
                hwnd: 0,
                object: crate::translate::win_events::OBJID_WINDOW,
            };
            if SENDER.with(|sink| sink.borrow().as_ref().map(|s| s.try_send(sentinel).is_ok()))
                == Some(true)
            {
                wake.notify();
            } else if SENDER.with(|sink| sink.borrow().is_some()) {
                let dropped = DROPPED.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                if dropped == 0 {
                    tracing::warn!("event channel full; monitor refresh dropped");
                }
            }
        }
        let _ = TranslateMessage(&msg);
        DispatchMessageW(&msg);
    }

    let _ = KillTimer(None, timer);
    let _ = UnhookWinEvent(hook);
}
