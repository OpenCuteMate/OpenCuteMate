//! Pure-translation fixture tests for the Windows provider.
//!
//! These run on **every OS** — they exercise the exact code the Win32
//! API layer feeds on Windows, with raw data shaped like real API
//! output. They do NOT touch a Windows session; runtime validation
//! happens on Windows with the `win-probe` example (see
//! `docs/desktop-awareness.md` §10 for the separation).

use ocm_desktop::OutputTransform;

use crate::translate::{
    monitor_to_info, monitors_to_info, physical_point_to_logical, physical_rect_to_logical,
    scale_from_dpi, transform_from_orientation, Orientation, RawMonitor,
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
