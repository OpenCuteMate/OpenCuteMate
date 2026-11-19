//! Translation between Niri's IPC JSON and the neutral
//! [`ocm_desktop`] model.
//!
//! The mirror types here pin the subset of the niri-ipc wire format this
//! crate consumes (niri-ipc 26.04 shape, verified against the live
//! compositor). Niri explicitly adds fields/variants across releases;
//! parsing therefore ignores unknown event variants and unknown struct
//! fields (serde's default behavior for `Option`-less structs is to
//! error on unknown fields — so every mirrored struct skips them).

use std::collections::HashMap;

use ocm_desktop::{
    OutputInfo, OutputPhysical, OutputTransform, WindowGeometry, WindowInfo, WindowState,
    WorkspaceInfo,
};
use serde::Deserialize;

// ---------------------------------------------------------------------------
// Mirror types (niri-ipc wire format)
// ---------------------------------------------------------------------------

/// `unknown fields are skipped` so new niri fields never break us.
#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)] // the mirror keeps the full wire shape for documentation
pub struct NiriOutput {
    pub name: String,
    pub make: String,
    pub model: String,
    #[serde(default)]
    pub modes: Vec<NiriMode>,
    #[serde(default)]
    pub current_mode: Option<usize>,
    #[serde(default)]
    pub logical: Option<NiriLogicalOutput>,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[allow(dead_code)] // wire-shape mirror; `is_preferred` is informational
pub struct NiriMode {
    pub width: u16,
    pub height: u16,
    pub refresh_rate: u32,
    #[serde(default)]
    pub is_preferred: bool,
}

#[derive(Debug, Clone, Copy, Deserialize)]
pub struct NiriLogicalOutput {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    pub scale: f64,
    pub transform: NiriTransform,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
pub enum NiriTransform {
    // Wire names exactly as niri-ipc serializes them: only the numeric
    // variants carry serde renames; everything else serializes as its
    // Rust variant name (verified against both a host output "Normal"
    // and a nested output "Flipped180").
    Normal,
    #[serde(rename = "90")]
    Rotate90,
    #[serde(rename = "180")]
    Rotate180,
    #[serde(rename = "270")]
    Rotate270,
    Flipped,
    Flipped90,
    Flipped180,
    Flipped270,
}

#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)] // wire-shape mirror; some fields are informational only
pub struct NiriWindowLayout {
    #[serde(default)]
    pub pos_in_scrolling_layout: Option<(usize, usize)>,
    pub tile_size: (f64, f64),
    pub window_size: (i32, i32),
    #[serde(default)]
    pub tile_pos_in_workspace_view: Option<(f64, f64)>,
    #[serde(default)]
    pub window_offset_in_tile: (f64, f64),
}

#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)] // wire-shape mirror; `pid` is informational here
pub struct NiriWindow {
    pub id: u64,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub app_id: Option<String>,
    #[serde(default)]
    pub pid: Option<i32>,
    #[serde(default)]
    pub workspace_id: Option<u64>,
    pub is_focused: bool,
    pub is_floating: bool,
    pub is_urgent: bool,
    pub layout: NiriWindowLayout,
}

#[derive(Debug, Clone, Deserialize)]
pub struct NiriWorkspace {
    pub id: u64,
    pub idx: u8,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub output: Option<String>,
    pub is_urgent: bool,
    pub is_active: bool,
    pub is_focused: bool,
    #[serde(default)]
    pub active_window_id: Option<u64>,
}

/// The event-stream variants this crate consumes (parsed from the wire's
/// `{"VariantName":{...}}` shape). Everything else parses to
/// [`NiriEvent::Ignored`].
#[derive(Debug, Clone)]
pub enum NiriEvent {
    WorkspacesChanged(Vec<NiriWorkspace>),
    WorkspaceActivated {
        id: u64,
        focused: bool,
    },
    WorkspaceActiveWindowChanged {
        workspace_id: u64,
        active_window_id: Option<u64>,
    },
    WindowsChanged(Vec<NiriWindow>),
    WindowOpenedOrChanged(NiriWindow),
    WindowClosed(u64),
    WindowFocusChanged(Option<u64>),
    WindowLayoutsChanged(Vec<(u64, NiriWindowLayout)>),
    /// Known but not represented in the neutral model (keyboard layout,
    /// overview, casts, urgency-only updates, config reloads, focus
    /// timestamps...), or unknown to this crate (forward compatibility).
    Ignored,
}

// ---------------------------------------------------------------------------
// Conversions
// ---------------------------------------------------------------------------

pub fn outputs_to_info(map: HashMap<String, NiriOutput>) -> Vec<OutputInfo> {
    let mut outputs: Vec<OutputInfo> = map.into_values().map(output_to_info).collect();
    outputs.sort_by(|a, b| a.id.cmp(&b.id));
    outputs
}

pub fn output_to_info(output: NiriOutput) -> OutputInfo {
    let NiriOutput {
        name,
        make,
        model,
        modes,
        current_mode,
        logical,
        ..
    } = output;
    // The current mode carries the physical resolution and refresh rate.
    let physical = current_mode.and_then(|index| modes.get(index)).map(|mode| {
        let _ = modes;
        OutputPhysical {
            resolution: (u32::from(mode.width), u32::from(mode.height)),
            refresh_mhz: Some(mode.refresh_rate),
        }
    });
    let (logical_position, logical_size, scale, transform) = match logical {
        Some(l) => (
            Some((l.x, l.y)),
            Some((l.width, l.height)),
            Some(l.scale),
            Some(transform_to_info(l.transform)),
        ),
        None => (None, None, None, None),
    };
    OutputInfo {
        id: name.clone(),
        // "make model" reads better than either part alone; serial is
        // rarely set and not needed for anything.
        name: format!("{make} {model}"),
        logical_position,
        logical_size,
        scale,
        transform,
        physical,
        // Niri's IPC exposes neither reserved areas nor a primary flag.
        work_area: None,
        primary: None,
    }
}

pub fn transform_to_info(transform: NiriTransform) -> OutputTransform {
    match transform {
        NiriTransform::Normal => OutputTransform::Normal,
        NiriTransform::Rotate90 => OutputTransform::Rotate90,
        NiriTransform::Rotate180 => OutputTransform::Rotate180,
        NiriTransform::Rotate270 => OutputTransform::Rotate270,
        NiriTransform::Flipped => OutputTransform::Flipped,
        NiriTransform::Flipped90 => OutputTransform::Flipped90,
        NiriTransform::Flipped180 => OutputTransform::Flipped180,
        NiriTransform::Flipped270 => OutputTransform::Flipped270,
    }
}

pub fn window_to_info(window: NiriWindow) -> WindowInfo {
    let NiriWindow {
        id,
        title,
        app_id,
        pid,
        workspace_id,
        is_focused,
        is_floating,
        is_urgent,
        layout,
    } = window;
    WindowInfo {
        id,
        title,
        app_id,
        // Niri types the pid as i32; a negative value would be
        // nonsensical, so map it to None rather than wrap.
        pid: pid.filter(|p| *p >= 0).map(|p| p as u32),
        workspace_id,
        state: WindowState {
            focused: Some(is_focused),
            // Not exposed by the IPC (checked niri-ipc 26.04): never
            // guessed here. Visibility would be derivable from the
            // workspace activeness, but niri does not report it
            // directly — left None like the flags above.
            minimized: None,
            maximized: None,
            fullscreen: None,
            floating: Some(is_floating),
            visible: None,
            urgent: Some(is_urgent),
        },
        geometry: geometry_from_layout(&layout),
    }
}

/// On-screen geometry from a niri window layout.
///
/// Semantics (verified against niri 26.04 sources and the live
/// compositor): `tile_pos_in_workspace_view` is the tile's position in
/// its workspace's view — which spans the whole output — and niri fills
/// it **only for floating windows** (scrolling-layout windows never get
/// a position; fullscreen toplevels fall under the same rule). Sizes
/// (`tile_size`, including compositor decorations such as borders) are
/// always present. Without a position there is no geometry to report:
/// `None`, never a guess.
pub fn geometry_from_layout(layout: &NiriWindowLayout) -> Option<WindowGeometry> {
    let (x, y) = layout.tile_pos_in_workspace_view?;
    Some(WindowGeometry {
        x,
        y,
        width: layout.tile_size.0,
        height: layout.tile_size.1,
    })
}

/// Rebuild a cached window's geometry after a layout-only change event.
pub fn apply_layout(base: &WindowInfo, layout: &NiriWindowLayout) -> WindowInfo {
    WindowInfo {
        geometry: geometry_from_layout(layout),
        ..base.clone()
    }
}

pub fn workspace_to_info(workspace: &NiriWorkspace) -> WorkspaceInfo {
    WorkspaceInfo {
        id: workspace.id,
        index: Some(workspace.idx),
        name: workspace.name.clone(),
        output: workspace.output.clone(),
        active: Some(workspace.is_active),
        focused: Some(workspace.is_focused),
        urgent: Some(workspace.is_urgent),
        active_window_id: workspace.active_window_id,
    }
}

// ---------------------------------------------------------------------------
// Event-line parsing
// ---------------------------------------------------------------------------

/// Parse one event-stream line (`{"Variant":{...}}`) into a
/// [`NiriEvent`]. Unknown variants are [`NiriEvent::Ignored`] (niri adds
/// variants across releases; skipping is the correct response).
pub fn parse_event_line(line: &str) -> Result<NiriEvent, serde_json::Error> {
    let value: serde_json::Value = serde_json::from_str(line)?;
    let serde_json::Value::Object(map) = value else {
        return Ok(NiriEvent::Ignored);
    };
    let Some((variant, payload)) = map.into_iter().next() else {
        return Ok(NiriEvent::Ignored);
    };
    let event = match variant.as_str() {
        "WorkspacesChanged" => NiriEvent::WorkspacesChanged(from_field(&payload, "workspaces")?),
        "WorkspaceActivated" => {
            let v: WorkspaceActivatedPayload = from_value(&payload)?;
            NiriEvent::WorkspaceActivated {
                id: v.id,
                focused: v.focused,
            }
        }
        "WorkspaceActiveWindowChanged" => {
            let v: WorkspaceActiveWindowPayload = from_value(&payload)?;
            NiriEvent::WorkspaceActiveWindowChanged {
                workspace_id: v.workspace_id,
                active_window_id: v.active_window_id,
            }
        }
        "WindowsChanged" => NiriEvent::WindowsChanged(from_field(&payload, "windows")?),
        "WindowOpenedOrChanged" => {
            NiriEvent::WindowOpenedOrChanged(from_field(&payload, "window")?)
        }
        "WindowClosed" => NiriEvent::WindowClosed(from_value::<IdPayload>(&payload)?.id),
        "WindowFocusChanged" => {
            NiriEvent::WindowFocusChanged(from_value::<FocusPayload>(&payload)?.id)
        }
        "WindowLayoutsChanged" => NiriEvent::WindowLayoutsChanged(from_field(&payload, "changes")?),
        // Keyboard layout switches, overview, casts, screenshots, config
        // reloads, urgency-only and focus-timestamp updates: known but
        // not part of the neutral model yet.
        _ => NiriEvent::Ignored,
    };
    Ok(event)
}

/// serde_json::from_value with the error type we propagate.
fn from_value<T: for<'de> Deserialize<'de>>(
    value: &serde_json::Value,
) -> Result<T, serde_json::Error> {
    serde_json::from_value(value.clone())
}

/// A named list field inside the event payload (niri wraps list payloads
/// in a single-key object: `{"workspaces": [...]}`).
fn from_field<T: for<'de> Deserialize<'de>>(
    payload: &serde_json::Value,
    field: &str,
) -> Result<T, serde_json::Error> {
    from_value(&payload[field])
}

#[derive(Deserialize)]
struct WorkspaceActivatedPayload {
    id: u64,
    focused: bool,
}

#[derive(Deserialize)]
struct WorkspaceActiveWindowPayload {
    workspace_id: u64,
    active_window_id: Option<u64>,
}

#[derive(Deserialize)]
struct IdPayload {
    id: u64,
}

#[derive(Deserialize)]
struct FocusPayload {
    id: Option<u64>,
}
