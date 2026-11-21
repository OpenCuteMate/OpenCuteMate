//! # ocm-platform-windows
//!
//! Native Windows platform backend for OpenCuteMate: a per-monitor,
//! borderless, topmost, non-activating overlay window with click-through
//! outside the character, mouse input translated into the neutral
//! [`ocm_platform::PointerEvent`] vocabulary, and a Win32 message loop
//! with a high-resolution waitable timer for the scheduler's deadlines.
//!
//! ## Architecture (mirrors `ocm-platform-wayland`)
//!
//! - [`WindowsPlatform`] owns process-level setup (DPI awareness, the
//!   waitable timer) and creates overlays.
//! - [`WindowsOverlay`] is the `OverlaySurface` implementation: one
//!   monitor-sized `WS_POPUP` window. The renderer attaches to it through
//!   `raw-window-handle` exactly like on Wayland; there is no second
//!   window.
//! - The event loop is *pull*-driven by the runtime driver:
//!   `set_deadline` + `wait_and_dispatch` block until a message, a
//!   scheduler deadline or an external wake (desktop-provider events)
//!   arrives. Idle means zero wakeups, same as the Wayland build.
//!
//! ## Transparency
//!
//! Per-pixel alpha is preferred: the window is created with
//! `WS_EX_NOREDIRECTIONBITMAP` and the renderer negotiates a compositing
//! alpha mode. When the GPU backend offers only `Opaque` (wgpu's DX12
//! HWND surfaces do), the runtime switches to the documented **color-key
//! fallback**: `WS_EX_LAYERED` + `SetLayeredWindowAttributes(LWA_COLORKEY)`
//! makes the renderer's clear color transparent. Keying is binary — the
//! soft shadow cannot survive it and is disabled — but transparency is
//! never silently broken: the chosen mode is logged and reported by
//! `win-overlay-probe`.
//!
//! ## Input
//!
//! Click-through is per-point, not per-window: `WM_NCHITTEST` consults the
//! character's current logical input region (the same rectangles the
//! renderer uses for hitboxes) and answers `HTTRANSPARENT` outside them,
//! `HTCLIENT` inside. The window never activates (`WS_EX_NOACTIVATE`), so
//! clicking the character cannot steal focus from the user's application.
//!
//! ## Coordinates
//!
//! The process is per-monitor-DPI-aware (v2). Win32 reports physical
//! pixels; the overlay exposes the logical frame `physical / scale`
//! (scale = DPI / 96) so the character, physics, hitboxes and camera use
//! the same logical space they use on Wayland. The only conversions happen
//! in `hittest` (unit-tested on every OS) and `ScreenToClient` inside the
//! window procedure.

mod hittest;

#[cfg(windows)]
mod win;

#[cfg(windows)]
pub use win::{WakeHandle, Wakeup, WindowsOverlay, WindowsPlatform};
