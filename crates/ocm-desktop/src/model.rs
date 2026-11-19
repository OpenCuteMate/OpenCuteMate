//! The neutral data model of the desktop environment.
//!
//! Every field that a compositor may not expose is `Option`. A provider
//! fills what it actually knows; consumers treat `None` as "this
//! compositor does not report it" — never as a default value. The
//! capability report ([`DesktopCapabilities`]) states up front which
//! families of information a provider can supply at all.

use ocm_platform::Rect;

/// One connected display in the compositor's global layout.
///
/// `id` is the stable per-session identifier (the Wayland/IPC output
/// name, e.g. `"eDP-1"`); it is what providers and consumers key on.
/// All geometry fields are in **logical pixels** in the global desktop
/// coordinate space (see `docs/desktop-awareness.md` §3).
#[derive(Debug, Clone, PartialEq)]
pub struct OutputInfo {
    /// Stable identifier (output name).
    pub id: String,
    /// Human-readable name (manufacturer/model or description), if known.
    pub name: String,
    /// Logical position of the output's top-left corner in the global
    /// desktop space. May be negative (outputs left of / above the
    /// origin). `None` when the compositor does not expose a layout.
    pub logical_position: Option<(i32, i32)>,
    /// Logical size in px. `None` when unknown (e.g. a disabled output).
    pub logical_size: Option<(u32, u32)>,
    /// Scale factor (1.0, 1.25, 2.0, ...). Fractional scales are exact
    /// values, not rounded integers.
    pub scale: Option<f64>,
    /// Rotation/flip applied to the output, if reported.
    pub transform: Option<OutputTransform>,
    /// Physical mode (resolution / refresh), if reported.
    pub physical: Option<OutputPhysical>,
    /// Portion of the output usable by normal windows (the area left
    /// after panels and other reserved strips), in **output-local**
    /// logical coordinates. `None` when the compositor does not expose
    /// reserved areas — consumers must then treat the whole output as
    /// usable.
    pub work_area: Option<Rect>,
    /// Whether this is the "primary" output, when the compositor has
    /// such a concept. `None` = no such concept.
    pub primary: Option<bool>,
}

impl OutputInfo {
    /// An output known only by its identifier (all geometry unknown).
    pub fn unidentified(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            name: String::new(),
            logical_position: None,
            logical_size: None,
            scale: None,
            transform: None,
            physical: None,
            work_area: None,
            primary: None,
        }
    }
}

/// Physical resolution and refresh rate of an output's current mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OutputPhysical {
    /// Resolution in physical (device) pixels.
    pub resolution: (u32, u32),
    /// Refresh rate in millihertz (144003 = 144.003 Hz), if reported.
    pub refresh_mhz: Option<u32>,
}

/// Output rotation/flip (matching the Wayland `wl_output.transform`
/// naming; rotations are counter-clockwise).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum OutputTransform {
    #[default]
    Normal,
    Rotate90,
    Rotate180,
    Rotate270,
    Flipped,
    Flipped90,
    Flipped180,
    Flipped270,
}

/// A window rectangle in **global logical** coordinates (fractional; the
/// compositor may place windows on sub-pixel logical positions, e.g. a
/// 1.25× output rendering a 2 px border as 1.6 logical px).
///
/// For floating windows this is the full visual tile (window plus
/// compositor decorations); the exact semantic is provider-specific and
/// documented there.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WindowGeometry {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

impl WindowGeometry {
    /// Whether the (global logical) point is inside the rectangle.
    /// Right/bottom edges are exclusive so adjacent rectangles tile
    /// without double-counting their shared edge.
    pub fn contains(&self, point: (f64, f64)) -> bool {
        point.0 >= self.x
            && point.0 < self.x + self.width
            && point.1 >= self.y
            && point.1 < self.y + self.height
    }

    /// Integer rounding for consumers that need an [`ocm_platform::Rect`]
    /// (origin floored, extent rounded). Display-only; spatial math must
    /// use the exact `f64` values to avoid drift.
    pub fn round_rect(&self) -> Rect {
        Rect {
            x: self.x.floor() as i32,
            y: self.y.floor() as i32,
            width: self.width.round() as i32,
            height: self.height.round() as i32,
        }
    }
}

/// The state flags of one window. `None` = the compositor does not
/// report this property (there is no honest default).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct WindowState {
    pub focused: Option<bool>,
    pub minimized: Option<bool>,
    pub maximized: Option<bool>,
    pub fullscreen: Option<bool>,
    /// Whether the window is in a floating (untiled) layout. Only
    /// meaningful for compositors with both tiling and floating layouts;
    /// `None` for plain floating WMs.
    pub floating: Option<bool>,
    pub urgent: Option<bool>,
}

/// One toplevel window as reported by the provider.
///
/// `id` is stable for the window's lifetime (provider-specific space;
/// numeric ids from IPC APIs).
#[derive(Debug, Clone, PartialEq)]
pub struct WindowInfo {
    pub id: u64,
    pub title: Option<String>,
    pub app_id: Option<String>,
    /// Workspace the window is on (id into [`WorkspaceInfo`]), if known.
    pub workspace_id: Option<u64>,
    pub state: WindowState,
    /// On-screen geometry in global logical coordinates. `None` when the
    /// compositor does not expose a position for this window — e.g. Niri
    /// reports pixel positions only for floating windows (verified:
    /// niri 26.04 scrolling-layout windows never get one).
    pub geometry: Option<WindowGeometry>,
}

/// One workspace (virtual desktop), when the compositor has them.
#[derive(Debug, Clone, PartialEq)]
pub struct WorkspaceInfo {
    /// Stable id (does not change when the workspace moves).
    pub id: u64,
    /// Position of the workspace on its output's workspace strip; not
    /// stable across reordering. `None` when unknown.
    pub index: Option<u8>,
    pub name: Option<String>,
    /// Output the workspace currently lives on, if known.
    pub output: Option<String>,
    /// Active = currently shown on its output. `None` when unknown.
    pub active: Option<bool>,
    /// Focused = the single focused workspace across all outputs.
    pub focused: Option<bool>,
    pub urgent: Option<bool>,
    /// The active window on this workspace, if known.
    pub active_window_id: Option<u64>,
}

/// Complete environmental state at one point in time.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct DesktopSnapshot {
    pub outputs: Vec<OutputInfo>,
    pub windows: Vec<WindowInfo>,
    pub workspaces: Vec<WorkspaceInfo>,
}

/// What a provider can and cannot report.
///
/// `false` is a hard statement: "this provider never supplies it". It
/// lets consumers plan around gaps instead of probing. Each field maps
/// to the corresponding `Option`/event family in the model.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DesktopCapabilities {
    // --- outputs -------------------------------------------------------------
    /// The set of outputs can be enumerated.
    pub outputs: bool,
    /// Logical position/size per output.
    pub output_logical_geometry: bool,
    /// Per-output scale factors.
    pub output_scale: bool,
    /// Physical resolution / refresh rate.
    pub output_mode: bool,
    /// Rotation/flip.
    pub output_transform: bool,
    /// Work areas (reserved-area-aware usable regions).
    pub work_areas: bool,
    /// A primary-output concept exists and is reported.
    pub primary_output: bool,
    /// Output changes are **pushed** as events (otherwise consumers must
    /// re-poll; see [`DesktopProvider::refresh_outputs`]).
    pub output_events: bool,

    // --- windows -------------------------------------------------------------
    /// The set of windows can be enumerated.
    pub windows: bool,
    /// Titles / application ids.
    pub window_identity: bool,
    /// The workspace a window is on.
    pub window_workspace: bool,
    /// Window focus (enumeration and focus-change events).
    pub window_focus: bool,
    /// On-screen geometry is available for at least *some* windows.
    pub window_geometry: bool,
    /// Geometry is available for *every* window (not just e.g. floating
    /// or visible ones).
    pub window_geometry_all: bool,
    /// Minimized/maximized/fullscreen flags.
    pub window_state_flags: bool,

    // --- workspaces / delivery ----------------------------------------------
    pub workspaces: bool,
    /// Workspace changes are pushed.
    pub workspace_events: bool,
    /// Window focus changes are pushed.
    pub focus_events: bool,
    /// A persistent push subscription exists (no polling needed for the
    /// covered event families).
    pub event_stream: bool,
}
