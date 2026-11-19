//! Pure-translation fixture tests for the Windows provider.
//!
//! These run on **every OS** — they exercise the exact code the Win32
//! API layer feeds on Windows, with raw data shaped like real API
//! output. They do NOT touch a Windows session; runtime validation
//! happens on Windows with the `win-probe` example (see
//! `docs/desktop-awareness.md` §10 for the separation).

use ocm_desktop::OutputTransform;

use crate::translate::{
    is_relevant_window, map_win_event, monitor_to_info, monitors_to_info,
    physical_point_to_logical, physical_rect_to_logical, scale_from_dpi,
    transform_from_orientation, win_events, window_to_info, EventAction, Orientation, RawMonitor,
    RawWinEvent, RawWindow,
};

/// A monitor shaped like real `GetMonitorInfoW`/`EnumDisplaySettingsW`
/// output on a typical 2560×1440 laptop panel at 150%.
fn laptop_monitor() -> RawMonitor {
    RawMonitor {
        device: "\\\\.\\DISPLAY1".to_string(),
        full: (0, 0, 2560, 1440),
        work: (0, 0, 2560, 1380), // 60 px taskbar
        dpi: 144,                 // 150%
        primary: true,
        orientation: Orientation::Default,
        resolution: (2560, 1440),
        refresh_hz: Some(165),
    }
}

#[test]
fn dpi_maps_to_scale_factors() {
    assert_eq!(scale_from_dpi(96), 1.0);
    assert_eq!(scale_from_dpi(144), 1.5);
    assert_eq!(scale_from_dpi(120), 1.25);
    assert_eq!(scale_from_dpi(192), 2.0);
    assert_eq!(scale_from_dpi(0), 1.0, "degenerate DPI falls back to 100%");
}

#[test]
fn physical_to_logical_is_exact_division() {
    assert_eq!(
        physical_point_to_logical((1920, 1080), 1.25),
        (1536.0, 864.0)
    );
    assert_eq!(
        physical_point_to_logical((3000, 1500), 1.5),
        (2000.0, 1000.0)
    );
    let rect = physical_rect_to_logical((100, 50, 1100, 450), 2.0);
    assert_eq!(rect, (50.0, 25.0, 500.0, 200.0));
}

#[test]
fn windows_orientation_maps_to_wayland_counter_clockwise() {
    // Windows counts clockwise, the neutral model counter-clockwise.
    assert_eq!(
        transform_from_orientation(Orientation::Default),
        OutputTransform::Normal
    );
    assert_eq!(
        transform_from_orientation(Orientation::Cw90),
        OutputTransform::Rotate270
    );
    assert_eq!(
        transform_from_orientation(Orientation::Cw180),
        OutputTransform::Rotate180
    );
    assert_eq!(
        transform_from_orientation(Orientation::Cw270),
        OutputTransform::Rotate90
    );
}

#[test]
fn laptop_monitor_converts_with_150_percent_scale() {
    let output = monitor_to_info(&laptop_monitor());
    assert_eq!(output.id, "\\\\.\\DISPLAY1");
    // 2560/1.5 ≈ 1706.67 logical — the logical size the API surface
    // would report on a 150% monitor (fractional like Wayland's).
    assert_eq!(output.logical_position, Some((0, 0)));
    let (w, h) = output.logical_size.expect("size known");
    assert!((w as f64 - 2560.0 / 1.5).abs() < 1.0);
    assert!((h as f64 - 1440.0 / 1.5).abs() < 1.0);
    assert_eq!(output.scale, Some(1.5));
    // Taskbar: 60 physical px / 1.5 = 40 logical px reserved at the
    // bottom; the work area is output-local.
    let work = output.work_area.expect("work area known");
    assert_eq!(work.x, 0);
    assert_eq!(work.y, 0);
    assert!((work.height as f64 - (1380.0 / 1.5)).abs() < 1.0);
    assert_eq!(output.primary, Some(true));
    let physical = output.physical.expect("mode known");
    assert_eq!(physical.resolution, (2560, 1440));
    assert_eq!(physical.refresh_mhz, Some(165_000));
}

#[test]
fn secondary_monitor_with_negative_coordinates_and_mixed_dpi() {
    // A 100% 1920×1080 monitor placed left of and above a 150% one:
    // the classic mixed-DPI, negative-coordinate arrangement.
    let secondary = RawMonitor {
        device: "\\\\.\\DISPLAY2".to_string(),
        full: (-1920, -400, 0, 680),
        work: (-1920, -400, 0, 640), // 40 px taskbar
        dpi: 96,
        primary: false,
        orientation: Orientation::Default,
        resolution: (1920, 1080),
        refresh_hz: Some(60),
    };
    let output = monitor_to_info(&secondary);
    assert_eq!(output.logical_position, Some((-1920, -400)));
    assert_eq!(output.logical_size, Some((1920, 1080)));
    assert_eq!(output.scale, Some(1.0));
    let work = output.work_area.expect("work area");
    assert_eq!((work.x, work.y), (0, 0));
    assert_eq!((work.width, work.height), (1920, 1040));
    assert_eq!(output.primary, Some(false));

    // Both monitors together: the desktop is a collection of regions
    // (the shared DesktopLayout handles the rest and is tested there).
    let outputs = monitors_to_info(&[laptop_monitor(), secondary]);
    assert_eq!(outputs.len(), 2);
    assert_eq!(
        outputs[0].id, "\\\\.\\DISPLAY1",
        "sorted by id for stability"
    );
}

// ---------------------------------------------------------------------------
// Window translation fixtures
// ---------------------------------------------------------------------------

/// A foreground, visible, normal window shaped like real Win32 query
/// output on the 150% laptop monitor from above.
fn foreground_window() -> RawWindow {
    RawWindow {
        hwnd: 0x00A1B2C3,
        title: "Untitled - Notepad".to_string(),
        exe: Some("notepad".to_string()),
        pid: Some(4242),
        frame: (200, 100, 2360, 1100), // 2160×1000 physical
        visible: true,
        minimized: false,
        maximized: false,
        foreground: true,
        cloaked: false,
        tool_window: false,
        monitor_dpi: 144,
    }
}

#[test]
fn visible_window_converts_with_monitor_dpi() {
    let info = window_to_info(&foreground_window()).expect("relevant window");
    assert_eq!(info.id, 0x00A1B2C3);
    assert_eq!(info.title.as_deref(), Some("Untitled - Notepad"));
    assert_eq!(info.app_id.as_deref(), Some("notepad"));
    assert_eq!(info.pid, Some(4242));
    let geometry = info.geometry.expect("on-screen geometry");
    // 200/1.5 ≈ 133.3 logical — exact fractional, never rounded mid-way.
    assert!((geometry.x - 200.0 / 1.5).abs() < 1e-9);
    assert!((geometry.y - 100.0 / 1.5).abs() < 1e-9);
    assert!((geometry.width - (2360.0 - 200.0) / 1.5).abs() < 1e-9);
    assert_eq!(info.state.focused, Some(true));
    assert_eq!(info.state.minimized, Some(false));
    assert_eq!(info.state.maximized, Some(false));
    assert_eq!(info.state.visible, Some(true));
    // No honest Windows source: never guessed.
    assert_eq!(info.state.fullscreen, None);
    assert_eq!(info.state.floating, None);
    assert_eq!(info.state.urgent, None);
    assert_eq!(info.workspace_id, None);
}

#[test]
fn the_documented_filter_excludes_invisible_cloaked_and_tool_windows() {
    let mut hidden = foreground_window();
    hidden.visible = false;
    assert!(
        !is_relevant_window(&hidden),
        "invisible windows are excluded"
    );

    let mut cloaked = foreground_window();
    cloaked.cloaked = true;
    assert!(
        !is_relevant_window(&cloaked),
        "suspended UWP windows are excluded"
    );

    let mut tool = foreground_window();
    tool.tool_window = true;
    assert!(
        !is_relevant_window(&tool),
        "WS_EX_TOOLWINDOW windows are excluded"
    );

    // Everything else is kept — even odd-looking windows.
    assert!(is_relevant_window(&foreground_window()));
    let mut untitled = foreground_window();
    untitled.title = String::new();
    assert!(is_relevant_window(&untitled), "untitled windows are kept");
    assert_eq!(window_to_info(&untitled).unwrap().title, None);
}

#[test]
fn minimized_windows_have_no_geometry() {
    let mut minimized = foreground_window();
    minimized.minimized = true;
    // Win32 parks minimized windows at (-32000, -32000); that is not
    // an on-screen position, so none is reported.
    minimized.frame = (-32000, -32000, -31000, -31000);
    let info = window_to_info(&minimized).expect("still relevant");
    assert_eq!(info.geometry, None);
    assert_eq!(info.state.minimized, Some(true));
}

#[test]
fn states_distinguish_unsupported_from_false() {
    let mut maximized = foreground_window();
    maximized.maximized = true;
    maximized.foreground = false;
    let info = window_to_info(&maximized).unwrap();
    assert_eq!(info.state.maximized, Some(true));
    assert_eq!(info.state.focused, Some(false));
    // `None` means "the platform does not report it", never a default.
    assert_ne!(info.state.fullscreen, Some(false));
}

// ---------------------------------------------------------------------------
// WinEvent → action mapping fixtures
// ---------------------------------------------------------------------------

fn event(code: u32, hwnd: u64) -> RawWinEvent {
    RawWinEvent {
        code,
        hwnd,
        object: win_events::OBJID_WINDOW,
    }
}

#[test]
fn win_events_map_to_the_documented_actions() {
    use win_events::*;
    let h = 0x1234;
    // Everything that can change a window's record is a re-query.
    for code in [
        EVENT_OBJECT_CREATE,
        EVENT_OBJECT_SHOW,
        EVENT_OBJECT_HIDE,
        EVENT_OBJECT_LOCATIONCHANGE,
        EVENT_OBJECT_STATECHANGE,
        EVENT_SYSTEM_MINIMIZESTART,
        EVENT_SYSTEM_MINIMIZEEND,
    ] {
        assert_eq!(
            map_win_event(&event(code, h)),
            EventAction::RefreshWindow,
            "code {code:#x}"
        );
    }
    assert_eq!(
        map_win_event(&event(EVENT_OBJECT_DESTROY, h)),
        EventAction::WindowRemoved
    );
    assert_eq!(
        map_win_event(&event(EVENT_SYSTEM_FOREGROUND, h)),
        EventAction::Focused
    );
    assert_eq!(
        map_win_event(&event(crate::translate::PUMP_MONITOR_REFRESH, 0)),
        EventAction::MonitorRefresh
    );
}

#[test]
fn child_object_and_noise_events_are_ignored() {
    use win_events::*;
    // Child objects (menus, carets, controls) are the bulk of WinEvent
    // traffic and never window-level changes.
    assert_eq!(
        map_win_event(&RawWinEvent {
            code: EVENT_OBJECT_LOCATIONCHANGE,
            hwnd: 0x1234,
            object: -4, // OBJID_CARET
        }),
        EventAction::Ignore
    );
    // Unknown future codes must not become actions.
    assert_eq!(map_win_event(&event(0x9999, 0x1234)), EventAction::Ignore);
    assert_eq!(
        map_win_event(&event(EVENT_OBJECT_LOCATIONCHANGE, 0)),
        EventAction::Ignore
    );
}
