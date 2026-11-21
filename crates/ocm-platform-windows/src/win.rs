//! The cfg(windows) implementation: monitors, the overlay window, the
//! window procedure and the waitable-timer message loop.
//!
//! Panic discipline: the window procedure and Win32 callbacks are
//! `extern "system"` — a panic unwinding into them is undefined behavior.
//! They are written without unwraps or indexing, and shared state is
//! accessed through short `RefCell` borrows that never span a Win32 call
//! that can re-enter the procedure (SetWindowPos, DestroyWindow).

#![cfg(windows)]

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use ocm_core::PlatformError;
use ocm_platform::{
    MonitorInfo, OverlayParams, OverlaySurface, Platform, PlatformEvent, PointerButton,
    PointerEvent, PointerEventKind, Rect,
};
use raw_window_handle::{
    RawDisplayHandle, RawWindowHandle, Win32WindowHandle, WindowsDisplayHandle,
};

use windows::core::BOOL;
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{
    COLORREF, HANDLE, HWND, LPARAM, LRESULT, POINT, RECT, WAIT_EVENT, WAIT_OBJECT_0, WPARAM,
};
use windows::Win32::Graphics::Gdi::{
    EnumDisplayMonitors, GetMonitorInfoW, ScreenToClient, HDC, HMONITOR, MONITORINFOEXW,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::{
    CancelWaitableTimer, CreateWaitableTimerExW, SetWaitableTimer,
    CREATE_WAITABLE_TIMER_HIGH_RESOLUTION, INFINITE, TIMER_ALL_ACCESS,
};
use windows::Win32::UI::Controls::WM_MOUSELEAVE;
use windows::Win32::UI::HiDpi::{
    GetDpiForMonitor, SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
    MDT_EFFECTIVE_DPI,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    ReleaseCapture, SetCapture, TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetWindowLongPtrW,
    MsgWaitForMultipleObjectsEx, PeekMessageW, PostQuitMessage, RegisterClassW,
    SetLayeredWindowAttributes, SetWindowLongPtrW, SetWindowPos, ShowWindow, TranslateMessage,
    GWL_EXSTYLE, HTCLIENT, HTTRANSPARENT, HWND_TOPMOST, LWA_COLORKEY, MWMO_INPUTAVAILABLE,
    PM_REMOVE, QS_ALLINPUT, SWP_NOACTIVATE, SWP_SHOWWINDOW, WINDOW_STYLE, WM_CLOSE, WM_DESTROY,
    WM_DISPLAYCHANGE, WM_DPICHANGED, WM_ERASEBKGND, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MBUTTONDOWN,
    WM_MBUTTONUP, WM_MOUSEMOVE, WM_NCHITTEST, WM_PAINT, WM_QUIT, WM_RBUTTONDOWN, WM_RBUTTONUP,
    WNDCLASSW, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_NOREDIRECTIONBITMAP, WS_EX_TOOLWINDOW,
    WS_EX_TOPMOST, WS_POPUP, WS_VISIBLE,
};

use crate::hittest::{physical_size_to_logical, physical_to_logical, point_is_interactive};

/// Window class name (registered once per process).
const CLASS_NAME: PCWSTR = w!("OpenCuteMateOverlay");

/// What ended the wait in [`WindowsPlatform::wait_and_dispatch`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Wakeup {
    /// The scheduler deadline (waitable timer) expired.
    Timer,
    /// An external wake (desktop-awareness provider event).
    Wake,
    /// Window messages were dispatched.
    Messages,
    /// The message loop delivered `WM_QUIT` or the overlay closed.
    Quit,
}

/// A manual-reset-free (auto-reset) wake event the desktop provider can
/// signal from its hook thread, so provider events wake the loop without
/// polling.
pub struct WakeHandle {
    event: HANDLE,
    /// Raw value for the (Send + Sync) callback closure.
    raw: isize,
}

impl WakeHandle {
    /// The signal callback handed to a desktop provider.
    pub fn callback(&self) -> Arc<dyn Fn() + Send + Sync> {
        let raw = self.raw;
        Arc::new(move || unsafe {
            let _ =
                windows::Win32::System::Threading::SetEvent(HANDLE(raw as *mut core::ffi::c_void));
        })
    }

    fn handle(&self) -> HANDLE {
        self.event
    }
}

impl Drop for WakeHandle {
    fn drop(&mut self) {
        unsafe {
            let _ = windows::Win32::Foundation::CloseHandle(self.event);
        }
    }
}

/// A monitor as enumerated natively (physical virtual-screen
/// coordinates, per-monitor DPI).
#[derive(Debug, Clone)]
struct RawMonitor {
    device: String,
    rect: (i32, i32, i32, i32),
    dpi: u32,
    primary: bool,
}

impl RawMonitor {
    fn scale(&self) -> f64 {
        if self.dpi == 0 {
            1.0
        } else {
            self.dpi as f64 / 96.0
        }
    }
    fn size(&self) -> (i32, i32) {
        (self.rect.2 - self.rect.0, self.rect.3 - self.rect.1)
    }
}

/// Shared overlay state, reached by the window procedure through the
/// thread-local [`CURRENT`] (messages are delivered on the creating
/// thread).
struct Shared {
    hwnd: HWND,
    instance: isize,
    device: String,
    physical: (i32, i32, i32, i32),
    scale: f64,
    logical: (u32, u32),
    /// Logical input rectangles; `None` = the whole surface is interactive.
    input: Option<Vec<Rect>>,
    pending: Vec<PlatformEvent>,
    closed: bool,
    /// Pointer-inside tracking for synthesized Enter/Leave.
    inside: bool,
    /// Applied color key, when the fallback engaged (diagnostics).
    color_key: Option<(u8, u8, u8)>,
}

thread_local! {
    /// The current thread's overlay state (one overlay per thread).
    static CURRENT: RefCell<Option<Rc<RefCell<Shared>>>> = const { RefCell::new(None) };
}

/// Platform handle: owns the high-resolution waitable timer.
pub struct WindowsPlatform {
    timer: HANDLE,
}

impl Platform for WindowsPlatform {
    type Overlay = WindowsOverlay;
    type Error = PlatformError;

    fn connect() -> Result<Self, PlatformError> {
        unsafe {
            // Per-monitor-v2 awareness: Win32 rects are true physical
            // pixels; the overlay converts to the logical model.
            if let Err(e) =
                SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2)
            {
                // A manifest or a parent API may have set it already; the
                // app still runs (coordinates follow the active mode).
                tracing::warn!("could not set per-monitor DPI awareness: {e}");
            }
        }
        let timer = unsafe {
            CreateWaitableTimerExW(
                None,
                PCWSTR::null(),
                CREATE_WAITABLE_TIMER_HIGH_RESOLUTION,
                TIMER_ALL_ACCESS.0,
            )
        }
        .map_err(|e| PlatformError::Other(format!("create waitable timer: {e}")))?;
        tracing::debug!("win32 platform ready (per-monitor DPI, high-resolution timer)");
        Ok(Self { timer })
    }

    fn monitors(&self) -> Vec<MonitorInfo> {
        monitors().into_iter().map(monitor_info).collect()
    }

    fn create_overlay(&mut self, params: &OverlayParams) -> Result<WindowsOverlay, PlatformError> {
        create_overlay(params)
    }
}

impl WindowsPlatform {
    /// A wake event for providers that push events from another thread.
    pub fn wake_handle(&self) -> Result<WakeHandle, PlatformError> {
        let event =
            unsafe { windows::Win32::System::Threading::CreateEventW(None, false, false, None) }
                .map_err(|e| PlatformError::Other(format!("create wake event: {e}")))?;
        Ok(WakeHandle {
            event,
            raw: event.0 as isize,
        })
    }

    /// Arm (or cancel) the scheduler deadline. Relative, 100 ns units.
    pub fn set_deadline(&self, after: Option<Duration>) -> Result<(), PlatformError> {
        unsafe {
            match after {
                Some(delay) => {
                    // Negative = relative. Clamp to >= 1 unit; 0 would mean
                    // "no due date" for SetWaitableTimer.
                    let units = (delay.as_nanos() / 100).max(1) as i64;
                    let due = -units;
                    SetWaitableTimer(self.timer, &due, 0, None, None, false)
                        .map_err(|e| PlatformError::Other(format!("set waitable timer: {e}")))
                }
                None => CancelWaitableTimer(self.timer)
                    .map_err(|e| PlatformError::Other(format!("cancel waitable timer: {e}"))),
            }
        }
    }

    /// Block until window messages, the deadline timer or an external
    /// wake arrives; dispatch messages and hand platform events to
    /// `on_event`. No timeout: parking is `INFINITE` with no timer.
    pub fn wait_and_dispatch(
        &mut self,
        wake: Option<&WakeHandle>,
        mut on_event: impl FnMut(PlatformEvent),
    ) -> Result<Wakeup, PlatformError> {
        let mut handles = vec![self.timer];
        if let Some(wake) = wake {
            handles.push(wake.handle());
        }

        let result = unsafe {
            MsgWaitForMultipleObjectsEx(Some(&handles), INFINITE, QS_ALLINPUT, MWMO_INPUTAVAILABLE)
        };

        // Drain every queued message; WM_QUIT ends the loop.
        let mut quit = false;
        let mut msg = windows::Win32::UI::WindowsAndMessaging::MSG::default();
        loop {
            let has = unsafe { PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() };
            if !has {
                break;
            }
            if msg.message == WM_QUIT {
                quit = true;
                break;
            }
            unsafe {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }

        // Deliver events the window procedure collected.
        let events = CURRENT.with(|current| {
            let shared = current.borrow().clone();
            match shared {
                Some(shared) => std::mem::take(&mut shared.borrow_mut().pending),
                None => Vec::new(),
            }
        });
        for event in events {
            on_event(event);
        }
        let closed = CURRENT.with(|current| {
            current
                .borrow()
                .as_ref()
                .map(|shared| shared.borrow().closed)
                .unwrap_or(true)
        });
        if quit || closed {
            return Ok(Wakeup::Quit);
        }

        if result == WAIT_OBJECT_0 {
            Ok(Wakeup::Timer)
        } else if result == WAIT_EVENT(WAIT_OBJECT_0.0 + 1) && wake.is_some() {
            Ok(Wakeup::Wake)
        } else {
            Ok(Wakeup::Messages)
        }
    }
}

impl Drop for WindowsPlatform {
    fn drop(&mut self) {
        unsafe {
            let _ = windows::Win32::Foundation::CloseHandle(self.timer);
        }
    }
}

/// A live overlay window on one monitor.
#[derive(Clone)]
pub struct WindowsOverlay {
    shared: Rc<RefCell<Shared>>,
}

impl WindowsOverlay {
    /// Diagnostics for logs and the probe tool.
    pub fn diagnostics(&self) -> OverlayDiagnostics {
        let s = self.shared.borrow();
        OverlayDiagnostics {
            device: s.device.clone(),
            physical_rect: s.physical,
            logical_size: s.logical,
            scale: s.scale,
            color_key: s.color_key,
        }
    }
}

/// Snapshot of the overlay's native configuration.
#[derive(Debug, Clone)]
pub struct OverlayDiagnostics {
    pub device: String,
    pub physical_rect: (i32, i32, i32, i32),
    pub logical_size: (u32, u32),
    pub scale: f64,
    pub color_key: Option<(u8, u8, u8)>,
}

impl OverlaySurface for WindowsOverlay {
    fn native_handles(&self) -> ocm_platform::NativeHandles {
        let s = self.shared.borrow();
        // SAFETY: the HWND lives until the window is destroyed, which only
        // happens after the runtime drops the renderer (documented teardown
        // order: renderer first, platform session last).
        let hwnd =
            std::num::NonZeroIsize::new(s.hwnd.0 as isize).expect("overlay HWND is non-null");
        let mut window = Win32WindowHandle::new(hwnd);
        window.hinstance = std::num::NonZeroIsize::new(s.instance);
        ocm_platform::NativeHandles {
            display: RawDisplayHandle::Windows(WindowsDisplayHandle::new()),
            window: RawWindowHandle::Win32(window),
        }
    }

    fn logical_size(&self) -> (u32, u32) {
        self.shared.borrow().logical
    }

    fn buffer_size(&self) -> (u32, u32) {
        let s = self.shared.borrow();
        (
            (s.physical.2 - s.physical.0).max(1) as u32,
            (s.physical.3 - s.physical.1).max(1) as u32,
        )
    }

    fn scale_factor(&self) -> f64 {
        self.shared.borrow().scale
    }

    fn set_input_region(&self, rects: Option<&[Rect]>) {
        // Hit-testing reads this immediately (WM_NCHITTEST); the window
        // itself keeps covering the monitor so the character can render
        // anywhere with per-pixel transparency.
        self.shared.borrow_mut().input = rects.map(|r| r.to_vec());
    }

    fn request_frame(&self) -> bool {
        // Win32 has no presentation callback; the scheduler arms its own
        // deadlines (see `supports_frame_callbacks`).
        false
    }

    fn commit(&self) {
        // Nothing to commit: the swapchain present is the commit on Win32.
    }

    fn target_output(&self) -> Option<String> {
        Some(self.shared.borrow().device.clone())
    }

    fn supports_frame_callbacks(&self) -> bool {
        false
    }

    fn set_color_key(&self, key: (u8, u8, u8)) {
        let (hwnd, instance) = {
            let s = self.shared.borrow();
            (s.hwnd, s.instance)
        };
        let color = COLORREF(u32::from(key.0) | (u32::from(key.1) << 8) | (u32::from(key.2) << 16));
        unsafe {
            // The fallback needs the layered-window path; a no-redirection
            // window has none, so drop that style (best effort — logged).
            let ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
            let layered = (ex & !(WS_EX_NOREDIRECTIONBITMAP.0 as isize)) | WS_EX_LAYERED.0 as isize;
            SetWindowLongPtrW(hwnd, GWL_EXSTYLE, layered);
            match SetLayeredWindowAttributes(hwnd, color, 255, LWA_COLORKEY) {
                Ok(()) => {
                    self.shared.borrow_mut().color_key = Some(key);
                    tracing::info!(
                        key = ?key,
                        "overlay uses the Opaque color-key fallback (binary transparency)"
                    );
                }
                Err(e) => tracing::error!("could not apply the color key: {e}"),
            }
            let _ = instance;
        }
    }
}

// ---------------------------------------------------------------------------
// Monitor enumeration
// ---------------------------------------------------------------------------

fn monitors() -> Vec<RawMonitor> {
    let mut found: Vec<RawMonitor> = Vec::new();
    let data = LPARAM(&mut found as *mut Vec<RawMonitor> as isize);
    unsafe {
        let _ = EnumDisplayMonitors(None, None, Some(monitor_proc), data);
    }
    found.sort_by(|a, b| a.device.cmp(&b.device));
    found
}

unsafe extern "system" fn monitor_proc(
    hmonitor: HMONITOR,
    _hdc: HDC,
    _rect: *mut RECT,
    lparam: LPARAM,
) -> BOOL {
    let found = &mut *(lparam.0 as *mut Vec<RawMonitor>);
    let mut info = MONITORINFOEXW::default();
    info.monitorInfo.cbSize = std::mem::size_of::<MONITORINFOEXW>() as u32;
    if !GetMonitorInfoW(hmonitor, &mut info as *mut _ as *mut _).as_bool() {
        return true.into();
    }
    let mut dpi_x: u32 = 0;
    let mut dpi_y: u32 = 0;
    let dpi = if GetDpiForMonitor(hmonitor, MDT_EFFECTIVE_DPI, &mut dpi_x, &mut dpi_y).is_ok()
        && dpi_x > 0
    {
        dpi_x
    } else {
        96
    };
    let device: Vec<u16> = info
        .szDevice
        .iter()
        .copied()
        .take_while(|c| *c != 0)
        .collect();
    let monitor = &info.monitorInfo;
    found.push(RawMonitor {
        device: String::from_utf16_lossy(&device),
        rect: (
            monitor.rcMonitor.left,
            monitor.rcMonitor.top,
            monitor.rcMonitor.right,
            monitor.rcMonitor.bottom,
        ),
        dpi,
        primary: (monitor.dwFlags & windows::Win32::UI::WindowsAndMessaging::MONITORINFOF_PRIMARY)
            != 0,
    });
    true.into()
}

fn monitor_info(raw: RawMonitor) -> MonitorInfo {
    let scale = raw.scale();
    let (w, h) = raw.size();
    let logical = physical_size_to_logical((w, h), scale);
    MonitorInfo {
        name: raw.device.clone(),
        description: raw.device,
        logical_position: Some((
            (raw.rect.0 as f64 / scale).round() as i32,
            (raw.rect.1 as f64 / scale).round() as i32,
        )),
        logical_size: Some(logical),
        scale: scale.round().max(1.0) as i32,
    }
}

// ---------------------------------------------------------------------------
// Overlay creation
// ---------------------------------------------------------------------------

fn create_overlay(params: &OverlayParams) -> Result<WindowsOverlay, PlatformError> {
    let all = monitors();
    if all.is_empty() {
        return Err(PlatformError::Other("no monitors enumerated".into()));
    }
    let target = pick_monitor(&all, &params.output)?;
    let (w, h) = target.size();
    let scale = target.scale();
    let logical = physical_size_to_logical((w, h), scale);

    unsafe {
        register_class_once()?;
        let instance = GetModuleHandleW(None)
            .map_err(|e| PlatformError::Other(format!("module handle: {e}")))?;
        let ex_style =
            WS_EX_TOPMOST | WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW | WS_EX_NOREDIRECTIONBITMAP;
        let hwnd = CreateWindowExW(
            ex_style,
            CLASS_NAME,
            w!("OpenCuteMate"),
            WINDOW_STYLE(WS_POPUP.0 | WS_VISIBLE.0),
            target.rect.0,
            target.rect.1,
            w,
            h,
            None,
            None,
            Some(windows::Win32::Foundation::HINSTANCE(instance.0)),
            None,
        )
        .map_err(|e| PlatformError::Other(format!("create overlay window: {e}")))?;

        // Topmost without activation; the class has no background brush so
        // nothing flashes before the first frame.
        let _ = ShowWindow(
            hwnd,
            windows::Win32::UI::WindowsAndMessaging::SW_SHOWNOACTIVATE,
        );
        let _ = SetWindowPos(
            hwnd,
            Some(HWND_TOPMOST),
            target.rect.0,
            target.rect.1,
            w,
            h,
            SWP_NOACTIVATE | SWP_SHOWWINDOW,
        );

        let shared = Rc::new(RefCell::new(Shared {
            hwnd,
            instance: instance.0 as isize,
            device: target.device.clone(),
            physical: target.rect,
            scale,
            logical,
            input: None,
            pending: Vec::new(),
            closed: false,
            inside: false,
            color_key: None,
        }));
        // Install after creation so WM_CREATE / WM_NCCREATE (which arrive
        // inside CreateWindowExW) fall through to DefWindowProc.
        CURRENT.with(|current| *current.borrow_mut() = Some(shared.clone()));

        tracing::info!(
            output = %target.device,
            physical = ?(w, h),
            logical = ?logical,
            scale,
            "overlay window created"
        );
        Ok(WindowsOverlay { shared })
    }
}

fn pick_monitor<'a>(all: &'a [RawMonitor], wanted: &str) -> Result<&'a RawMonitor, PlatformError> {
    if !wanted.is_empty() {
        return all
            .iter()
            .find(|m| m.device == wanted || m.device.contains(wanted))
            .ok_or_else(|| PlatformError::OutputNotFound(wanted.to_string()));
    }
    all.iter()
        .find(|m| m.primary)
        .or_else(|| all.first())
        .ok_or_else(|| PlatformError::Other("no monitors enumerated".into()))
}

fn register_class_once() -> Result<(), PlatformError> {
    static REGISTERED: std::sync::OnceLock<Result<(), String>> = std::sync::OnceLock::new();
    REGISTERED
        .get_or_init(|| {
            let instance =
                unsafe { GetModuleHandleW(None) }.map_err(|e| format!("module handle: {e}"))?;
            let class = WNDCLASSW {
                style: windows::Win32::UI::WindowsAndMessaging::CS_HREDRAW
                    | windows::Win32::UI::WindowsAndMessaging::CS_VREDRAW,
                lpfnWndProc: Some(wndproc),
                hInstance: windows::Win32::Foundation::HINSTANCE(instance.0),
                lpszClassName: CLASS_NAME,
                hbrBackground: windows::Win32::Graphics::Gdi::HBRUSH::default(),
                ..Default::default()
            };
            let atom = unsafe { RegisterClassW(&class) };
            if atom == 0 {
                Err("RegisterClassW failed".to_string())
            } else {
                Ok(())
            }
        })
        .clone()
        .map_err(PlatformError::Other)
}

// ---------------------------------------------------------------------------
// Window procedure
// ---------------------------------------------------------------------------

fn signed_x(lparam: LPARAM) -> i32 {
    (lparam.0 & 0xffff) as u16 as i16 as i32
}

fn signed_y(lparam: LPARAM) -> i32 {
    ((lparam.0 >> 16) & 0xffff) as u16 as i16 as i32
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let shared = CURRENT.with(|current| current.borrow().clone());

    match msg {
        WM_NCHITTEST => {
            // Per-point click-through: only the character's current input
            // region is interactive; everything else passes to the window
            // below (the user's applications keep working).
            let Some(shared) = shared else {
                return LRESULT(HTTRANSPARENT as isize);
            };
            let mut point = POINT {
                x: signed_x(lparam),
                y: signed_y(lparam),
            };
            if !ScreenToClient(hwnd, &mut point).as_bool() {
                return LRESULT(HTTRANSPARENT as isize);
            }
            let s = shared.borrow();
            let logical = physical_to_logical((point.x, point.y), s.scale);
            let interactive = point_is_interactive(logical, s.input.as_deref());
            drop(s);
            return LRESULT(if interactive {
                HTCLIENT as isize
            } else {
                HTTRANSPARENT as isize
            });
        }
        WM_ERASEBKGND => return LRESULT(1),
        WM_PAINT => {
            let mut ps = windows::Win32::Graphics::Gdi::PAINTSTRUCT::default();
            let _ = windows::Win32::Graphics::Gdi::BeginPaint(hwnd, &mut ps);
            let _ = windows::Win32::Graphics::Gdi::EndPaint(hwnd, &ps);
            return LRESULT(0);
        }
        WM_MOUSEMOVE => {
            if let Some(shared) = &shared {
                let mut s = shared.borrow_mut();
                let logical = physical_to_logical((signed_x(lparam), signed_y(lparam)), s.scale);
                if !s.inside {
                    s.inside = true;
                    s.pending.push(PlatformEvent::Pointer(PointerEvent {
                        kind: PointerEventKind::Enter,
                        x: logical.0,
                        y: logical.1,
                    }));
                }
                s.pending.push(PlatformEvent::Pointer(PointerEvent {
                    kind: PointerEventKind::Motion,
                    x: logical.0,
                    y: logical.1,
                }));
                drop(s);
                track_leave(hwnd);
            }
        }
        WM_MOUSELEAVE => {
            if let Some(shared) = &shared {
                let mut s = shared.borrow_mut();
                s.inside = false;
                s.pending.push(PlatformEvent::Pointer(PointerEvent {
                    kind: PointerEventKind::Leave,
                    x: 0.0,
                    y: 0.0,
                }));
            }
        }
        WM_LBUTTONDOWN | WM_LBUTTONUP | WM_RBUTTONDOWN | WM_RBUTTONUP | WM_MBUTTONDOWN
        | WM_MBUTTONUP => {
            if let Some(shared) = &shared {
                let (button, pressed) = match msg {
                    WM_LBUTTONDOWN => (PointerButton::Left, true),
                    WM_LBUTTONUP => (PointerButton::Left, false),
                    WM_RBUTTONDOWN => (PointerButton::Right, true),
                    WM_RBUTTONUP => (PointerButton::Right, false),
                    WM_MBUTTONDOWN => (PointerButton::Middle, true),
                    _ => (PointerButton::Middle, false),
                };
                let mut s = shared.borrow_mut();
                let logical = physical_to_logical((signed_x(lparam), signed_y(lparam)), s.scale);
                let kind = if pressed {
                    PointerEventKind::Press { button }
                } else {
                    PointerEventKind::Release { button }
                };
                s.pending.push(PlatformEvent::Pointer(PointerEvent {
                    kind,
                    x: logical.0,
                    y: logical.1,
                }));
                drop(s);
                // Capture keeps drag motion coming even across monitors.
                if pressed {
                    let _ = SetCapture(hwnd);
                } else {
                    let _ = ReleaseCapture();
                }
            }
        }
        WM_DPICHANGED => {
            // Suggested rect is in physical screen coordinates.
            let rect = *(lparam.0 as *const RECT);
            let resize = shared.as_ref().map(|shared| {
                let mut s = shared.borrow_mut();
                let dpi = ((wparam.0 >> 16) & 0xffff) as u32;
                if dpi > 0 {
                    s.scale = dpi as f64 / 96.0;
                }
                s.physical = (rect.left, rect.top, rect.right, rect.bottom);
                s.logical = physical_size_to_logical(
                    (rect.right - rect.left, rect.bottom - rect.top),
                    s.scale,
                );
                let (scale, logical) = (s.scale, s.logical);
                s.pending.push(PlatformEvent::ScaleChanged { scale });
                s.pending.push(PlatformEvent::Configure {
                    width: logical.0,
                    height: logical.1,
                });
                (rect.left, rect.top, rect.right, rect.bottom)
            });
            if let Some((l, t, r, b)) = resize {
                let _ = SetWindowPos(
                    hwnd,
                    Some(HWND_TOPMOST),
                    l,
                    t,
                    r - l,
                    b - t,
                    SWP_NOACTIVATE | SWP_SHOWWINDOW,
                );
            }
        }
        WM_DISPLAYCHANGE => {
            // Monitor layout/DPI may have changed: re-reconcile with the
            // monitor we cover (by device name; keep last geometry if it
            // vanished — a closed laptop lid must not corrupt the model).
            let device = shared
                .as_ref()
                .map(|shared| shared.borrow().device.clone())
                .unwrap_or_default();
            if !device.is_empty() {
                if let Some(monitor) = monitors().into_iter().find(|m| m.device == device) {
                    let (w, h) = monitor.size();
                    let resize = shared.as_ref().map(|shared| {
                        let mut s = shared.borrow_mut();
                        s.physical = monitor.rect;
                        s.scale = monitor.scale();
                        s.logical = physical_size_to_logical((w, h), s.scale);
                        let (scale, logical) = (s.scale, s.logical);
                        s.pending.push(PlatformEvent::ScaleChanged { scale });
                        s.pending.push(PlatformEvent::Configure {
                            width: logical.0,
                            height: logical.1,
                        });
                        monitor.rect
                    });
                    if let Some((l, t, _, _)) = resize {
                        let _ = SetWindowPos(
                            hwnd,
                            Some(HWND_TOPMOST),
                            l,
                            t,
                            w,
                            h,
                            SWP_NOACTIVATE | SWP_SHOWWINDOW,
                        );
                    }
                }
            }
        }
        WM_CLOSE => {
            if let Some(shared) = &shared {
                let mut s = shared.borrow_mut();
                s.closed = true;
                s.pending.push(PlatformEvent::Closed);
            }
            let _ = DestroyWindow(hwnd);
            return LRESULT(0);
        }
        WM_DESTROY => {
            if let Some(shared) = &shared {
                let mut s = shared.borrow_mut();
                if !s.closed {
                    s.closed = true;
                    s.pending.push(PlatformEvent::Closed);
                }
            }
            PostQuitMessage(0);
            return LRESULT(0);
        }
        _ => {}
    }

    DefWindowProcW(hwnd, msg, wparam, lparam)
}

fn track_leave(hwnd: HWND) {
    let mut track = TRACKMOUSEEVENT {
        cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
        dwFlags: TME_LEAVE,
        hwndTrack: hwnd,
        dwHoverTime: 0,
    };
    unsafe {
        let _ = TrackMouseEvent(&mut track);
    }
}
