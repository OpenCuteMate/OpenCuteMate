//! Platform-neutral geometry rules for the Windows overlay.
//!
//! These are the decisions that must be identical everywhere (and are
//! unit-tested on every OS): how a physical client point maps to the
//! renderer's logical space, and whether a point is inside the
//! character's current input region. The Win32 layer calls exactly these
//! functions, so the hit-test that decides click-through and the
//! pointer event the interaction system sees cannot disagree.

// The Win32 layer consumes these on Windows; elsewhere only the tests do.
#![cfg_attr(not(windows), allow(dead_code))]

use ocm_platform::Rect;

/// Physical client pixel → logical surface coordinate (exact division by
/// the per-monitor scale — the same convention the wgpu/Bevy camera and
/// hitboxes use).
pub fn physical_to_logical(point: (i32, i32), scale: f64) -> (f64, f64) {
    (point.0 as f64 / scale, point.1 as f64 / scale)
}

/// Whether a logical point lies inside any input rect.
///
/// Rectangles are treated as right/bottom-exclusive (the same convention
/// as the shared `DesktopLayout`), and the *whole* surface counts when the
/// region is `None` (click-through disabled).
pub fn point_is_interactive(point: (f64, f64), region: Option<&[Rect]>) -> bool {
    match region {
        None => true,
        Some(rects) => rects.iter().any(|rect| {
            point.0 >= rect.x as f64
                && point.0 < (rect.x + rect.width) as f64
                && point.1 >= rect.y as f64
                && point.1 < (rect.y + rect.height) as f64
        }),
    }
}

/// Physical monitor rect → logical size, rounded up (the renderer's
/// buffer is exactly the physical size; the logical frame is the divided
/// value).
pub fn physical_size_to_logical(size: (i32, i32), scale: f64) -> (u32, u32) {
    (
        ((size.0.max(1) as f64) / scale).round().max(1.0) as u32,
        ((size.1.max(1) as f64) / scale).round().max(1.0) as u32,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x: i32, y: i32, w: i32, h: i32) -> Rect {
        Rect {
            x,
            y,
            width: w,
            height: h,
        }
    }

    #[test]
    fn physical_points_divide_by_scale_exactly() {
        assert_eq!(physical_to_logical((1920, 1080), 1.25), (1536.0, 864.0));
        assert_eq!(physical_to_logical((3000, 1500), 1.5), (2000.0, 1000.0));
        assert_eq!(physical_to_logical((0, 0), 2.0), (0.0, 0.0));
        // Negative client coordinates can occur during a capture drag;
        // they convert arithmetically, never clamped.
        assert_eq!(physical_to_logical((-150, -75), 1.5), (-100.0, -50.0));
    }

    #[test]
    fn hit_test_matches_the_input_region() {
        let rects = [rect(100, 100, 200, 150), rect(400, 300, 50, 50)];
        // Inside.
        assert!(point_is_interactive((100.0, 100.0), Some(&rects)));
        assert!(point_is_interactive((299.9, 249.9), Some(&rects)));
        assert!(point_is_interactive((425.0, 325.0), Some(&rects)));
        // Right/bottom edges are exclusive, exactly like DesktopLayout.
        assert!(!point_is_interactive((300.0, 200.0), Some(&rects)));
        assert!(!point_is_interactive((450.0, 350.0), Some(&rects)));
        // Outside every rect: click-through.
        assert!(!point_is_interactive((10.0, 10.0), Some(&rects)));
        // Empty region: fully click-through.
        assert!(!point_is_interactive((0.0, 0.0), Some(&[])));
        // None = whole surface interactive (drag grab / click-through off).
        assert!(point_is_interactive((0.0, 0.0), None));
        assert!(point_is_interactive((10_000.0, 10_000.0), None));
    }

    #[test]
    fn a_150_percent_monitor_keeps_its_logical_frame() {
        assert_eq!(physical_size_to_logical((2560, 1440), 1.5), (1707, 960));
        assert_eq!(physical_size_to_logical((1920, 1080), 1.0), (1920, 1080));
        assert_eq!(physical_size_to_logical((3840, 2160), 2.0), (1920, 1080));
        // Degenerate input cannot produce a zero-sized surface.
        assert_eq!(physical_size_to_logical((0, 0), 1.0), (1, 1));
    }

    #[test]
    fn scaled_hit_testing_is_consistent_with_converted_points() {
        // A character hitbox stated in logical px on a 150% monitor: the
        // physical point at its center must hit, one just outside must not.
        let scale = 1.5;
        let hitbox = rect(600, 400, 300, 150); // logical
        let center_physical = (
            ((hitbox.x as f64 + 150.0) * scale) as i32,
            ((hitbox.y as f64 + 75.0) * scale) as i32,
        );
        let center = physical_to_logical(center_physical, scale);
        assert!(point_is_interactive(center, Some(&[hitbox])));
        assert!(!point_is_interactive((599.0, 475.0), Some(&[hitbox])));
    }
}
