//! [`DesktopLayout`]: the desktop as a **collection of output regions**,
//! with all coordinate-space conversions.
//!
//! ## Coordinate space conventions
//!
//! - **Global logical desktop space** — the compositor's output layout:
//!   top-left origin, y down, logical pixels. Outputs may sit at
//!   negative positions and may leave gaps; the desktop is NOT assumed
//!   to be one rectangle.
//! - **Output-local logical space** — relative to an output's logical
//!   position. A layer-shell surface covering its output (our case)
//!   uses exactly this space for pointer events and the camera, so
//!   *surface-local == output-local* for the overlay.
//! - **Physical pixels** — `logical × scale`, per output. Kept exact
//!   (`f64`) in all math; rounding happens only at buffer boundaries
//!   (buffer size = `ceil(logical × scale)`, matching
//!   `ocm-platform-wayland`'s `buffer_size_for`), never inside
//!   conversions — rounding inside the loop is how character positions
//!   drift.
//!
//! Point-in-output membership uses right/bottom-exclusive edges so a
//! point on the shared edge of adjacent outputs belongs to exactly one.

use crate::model::OutputInfo;

/// A region-based spatial model of the desktop.
///
/// Built from the outputs that have known logical geometry; outputs
/// without position/size cannot participate in spatial math and are
/// excluded (they remain visible in the environment's output list).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct DesktopLayout {
    outputs: Vec<OutputInfo>,
}

impl DesktopLayout {
    pub fn new(outputs: &[OutputInfo]) -> Self {
        let mut outputs: Vec<OutputInfo> = outputs
            .iter()
            .filter(|o| o.logical_position.is_some() && o.logical_size.is_some())
            .cloned()
            .collect();
        // Deterministic ordering: scan line by line, left to right. All
        // multi-output queries then break ties the same way.
        outputs.sort_by_key(|o| {
            let (x, y) = o.logical_position.unwrap_or((i32::MAX, i32::MAX));
            (y, x)
        });
        Self { outputs }
    }

    pub fn outputs(&self) -> &[OutputInfo] {
        &self.outputs
    }

    pub fn output(&self, id: &str) -> Option<&OutputInfo> {
        self.outputs.iter().find(|o| o.id == id)
    }

    /// The output whose region contains a global logical point.
    pub fn output_at(&self, global: (f64, f64)) -> Option<&OutputInfo> {
        self.outputs.iter().find(|o| {
            let (Some((ox, oy)), Some((ow, oh))) = (o.logical_position, o.logical_size) else {
                return false;
            };
            global.0 >= ox as f64
                && global.0 < (ox + ow as i32) as f64
                && global.1 >= oy as f64
                && global.1 < (oy + oh as i32) as f64
        })
    }

    /// The output nearest to a global point (distance to its region),
    /// plus the point clamped into that region. Useful for points in
    /// gaps between displays.
    pub fn nearest_output(&self, global: (f64, f64)) -> Option<(&OutputInfo, (f64, f64))> {
        self.outputs
            .iter()
            .map(|o| {
                let (ox, oy) = o.logical_position.unwrap_or_default();
                let (ow, oh) = o.logical_size.unwrap_or_default();
                let max = ((ox + ow as i32) as f64, (oy + oh as i32) as f64);
                let clamped = (
                    global.0.clamp(ox as f64, max.0),
                    global.1.clamp(oy as f64, max.1),
                );
                let dx = global.0 - clamped.0;
                let dy = global.1 - clamped.1;
                (o, clamped, dx * dx + dy * dy)
            })
            .min_by(|a, b| a.2.total_cmp(&b.2))
            .map(|(o, clamped, _)| (o, clamped))
    }

    /// Global logical point → output-local logical point.
    pub fn to_output_local(&self, output_id: &str, global: (f64, f64)) -> Option<(f64, f64)> {
        let o = self.output(output_id)?;
        let (ox, oy) = o.logical_position?;
        Some((global.0 - ox as f64, global.1 - oy as f64))
    }

    /// Output-local logical point → global logical point.
    pub fn to_global(&self, output_id: &str, local: (f64, f64)) -> Option<(f64, f64)> {
        let o = self.output(output_id)?;
        let (ox, oy) = o.logical_position?;
        Some((local.0 + ox as f64, local.1 + oy as f64))
    }

    /// Output-local logical point → output-local **physical** pixels
    /// (`logical × scale`, exact). Rounding to whole device pixels is a
    /// buffer-boundary decision and is deliberately not done here.
    pub fn to_physical(&self, output_id: &str, local: (f64, f64)) -> Option<(f64, f64)> {
        let scale = self.output(output_id)?.scale?;
        Some((local.0 * scale, local.1 * scale))
    }

    /// Clamp a global point into an output's region.
    pub fn clamp_to_output(&self, output_id: &str, global: (f64, f64)) -> Option<(f64, f64)> {
        let o = self.output(output_id)?;
        let (Some((ox, oy)), Some((ow, oh))) = (o.logical_position, o.logical_size) else {
            return None;
        };
        Some((
            global.0.clamp(ox as f64, (ox + ow as i32) as f64),
            global.1.clamp(oy as f64, (oy + oh as i32) as f64),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::OutputInfo;

    fn output(id: &str, x: i32, y: i32, w: u32, h: u32, scale: f64) -> OutputInfo {
        OutputInfo {
            id: id.to_string(),
            name: id.to_string(),
            logical_position: Some((x, y)),
            logical_size: Some((w, h)),
            scale: Some(scale),
            transform: None,
            physical: None,
            work_area: None,
            primary: None,
        }
    }

    #[test]
    fn single_output_roundtrip_is_exact() {
        let layout = DesktopLayout::new(&[output("eDP-1", 0, 0, 1536, 864, 1.25)]);
        let global = (733.5, 123.25);
        let local = layout.to_output_local("eDP-1", global).unwrap();
        assert_eq!(local, global);
        assert_eq!(layout.to_global("eDP-1", local).unwrap(), global);
    }

    #[test]
    fn negative_output_positions_convert() {
        let layout = DesktopLayout::new(&[output("left", -1920, -100, 1920, 1080, 1.0)]);
        let local = layout.to_output_local("left", (-1000.5, -50.0)).unwrap();
        assert_eq!(local, (919.5, 50.0));
        assert_eq!(layout.to_global("left", local).unwrap(), (-1000.5, -50.0));
        // A point inside the negative-offset output resolves to it.
        assert_eq!(layout.output_at((-500.0, 0.0)).unwrap().id, "left");
        // The global origin is NOT on this output.
        assert_eq!(layout.output_at((0.0, 0.0)), None);
    }

    #[test]
    fn side_by_side_outputs_split_at_the_shared_edge() {
        // 1536-wide left output, 1920-wide right output, touching edges.
        let layout = DesktopLayout::new(&[
            output("a", 0, 0, 1536, 864, 1.0),
            output("b", 1536, 0, 1920, 1080, 1.0),
        ]);
        assert_eq!(layout.output_at((1535.9, 100.0)).unwrap().id, "a");
        assert_eq!(layout.output_at((1536.0, 100.0)).unwrap().id, "b");
        assert_eq!(
            layout.to_output_local("b", (1600.0, 10.0)).unwrap(),
            (64.0, 10.0)
        );
    }

    #[test]
    fn stacked_outputs_and_vertical_edges() {
        let layout = DesktopLayout::new(&[
            output("top", 0, -1080, 1920, 1080, 1.0),
            output("bottom", 0, 0, 1920, 1080, 1.0),
        ]);
        assert_eq!(layout.output_at((100.0, -0.1)).unwrap().id, "top");
        assert_eq!(layout.output_at((100.0, 0.0)).unwrap().id, "bottom");
    }

    #[test]
    fn gaps_between_outputs_are_not_desktop() {
        // Two outputs with a 200 px horizontal gap (the desktop is NOT
        // one rectangle: x ∈ [1000, 1200) belongs to no output).
        let layout = DesktopLayout::new(&[
            output("a", 0, 0, 1000, 800, 1.0),
            output("b", 1200, 0, 1000, 800, 1.0),
        ]);
        assert_eq!(layout.output_at((1100.0, 400.0)), None);
        // The point maps to nothing and converts to nothing.
        assert_eq!(
            layout.to_output_local("b", (1100.0, 400.0)),
            Some((-100.0, 400.0)),
            "conversion is arithmetic, not containment"
        );
        // ...but containment-based helpers refuse it:
        assert_eq!(
            layout.clamp_to_output("a", (1100.0, 400.0)),
            Some((1000.0, 400.0))
        );
        let (nearest, clamped) = layout.nearest_output((1100.0, 400.0)).unwrap();
        assert_eq!(
            nearest.id, "a",
            "equidistant ties go to the first in scan order"
        );
        assert_eq!(clamped, (1000.0, 400.0));
    }

    #[test]
    fn nearest_output_picks_the_actually_closest_region() {
        let layout = DesktopLayout::new(&[
            output("a", 0, 0, 1000, 800, 1.0),
            output("b", 1200, 0, 1000, 800, 1.0),
        ]);
        let (nearest, _) = layout.nearest_output((1190.0, 400.0)).unwrap();
        assert_eq!(nearest.id, "b");
        let (nearest, _) = layout.nearest_output((1010.0, 400.0)).unwrap();
        assert_eq!(nearest.id, "a");
    }

    #[test]
    fn l_shaped_arrangement_has_no_bounding_box_assumption() {
        // Small output bottom-right; the union is an L shape.
        let layout = DesktopLayout::new(&[
            output("big", 0, 0, 1920, 1080, 1.0),
            output("small", 1920, 540, 800, 540, 1.0),
        ]);
        // Inside the "bounding box" but on no output (bottom-right notch).
        assert_eq!(layout.output_at((2000.0, 100.0)), None);
        assert_eq!(layout.output_at((2000.0, 800.0)).unwrap().id, "small");
        assert_eq!(layout.output_at((1000.0, 500.0)).unwrap().id, "big");
    }

    #[test]
    fn per_output_scale_converts_to_physical_exactly() {
        let layout = DesktopLayout::new(&[
            output("hi", 0, 0, 1536, 864, 1.25),
            output("lo", 1536, 0, 1920, 1080, 1.0),
        ]);
        // The dev machine's numbers: 1536×864 logical @ 1.25× = 1920×1080.
        assert_eq!(
            layout.to_physical("hi", (1536.0, 864.0)).unwrap(),
            (1920.0, 1080.0)
        );
        assert_eq!(layout.to_physical("hi", (12.0, 3.0)).unwrap(), (15.0, 3.75));
        assert_eq!(layout.to_physical("lo", (12.0, 3.0)).unwrap(), (12.0, 3.0));
        assert_eq!(layout.to_physical("missing", (1.0, 1.0)), None);
    }

    #[test]
    fn outputs_without_geometry_do_not_participate() {
        let mut unknown = output("ghost", 0, 0, 100, 100, 1.0);
        unknown.logical_position = None;
        unknown.logical_size = None;
        let layout = DesktopLayout::new(&[output("real", 0, 0, 100, 100, 1.0), unknown]);
        assert_eq!(layout.outputs().len(), 1);
        assert_eq!(layout.output("ghost"), None);
        assert_eq!(layout.output_at((50.0, 50.0)).unwrap().id, "real");
    }

    #[test]
    fn clamp_corners_stay_inside_the_region() {
        let layout = DesktopLayout::new(&[output("a", -100, -100, 200, 200, 1.0)]);
        assert_eq!(
            layout.clamp_to_output("a", (-500.0, 50.0)),
            Some((-100.0, 50.0))
        );
        assert_eq!(
            layout.clamp_to_output("a", (500.0, 500.0)),
            Some((100.0, 100.0))
        );
    }
}
