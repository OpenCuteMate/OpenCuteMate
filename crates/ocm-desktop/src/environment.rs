//! [`DesktopEnvironment`]: the accumulated environmental state.
//!
//! One struct owns the current view of the desktop. It consumes
//! [`EnvironmentEvent`]s (from any provider) and reports what changed,
//! so consumers never diff snapshots themselves and never see provider
//! specifics. Full-replacement events are diffed against the current
//! state so the [`EnvironmentChange`] flags only fire on real changes —
//! an event stream's up-front state flood after an identical explicit
//! snapshot produces an all-false summary.

use ocm_platform::Rect;

use crate::events::{EnvironmentChange, EnvironmentEvent};
use crate::layout::DesktopLayout;
use crate::model::{
    DesktopCapabilities, DesktopSnapshot, OutputInfo, WindowGeometry, WindowInfo, WorkspaceInfo,
};

/// The current, provider-fed view of the desktop environment.
#[derive(Debug, Clone)]
pub struct DesktopEnvironment {
    capabilities: DesktopCapabilities,
    outputs: Vec<OutputInfo>,
    windows: Vec<WindowInfo>,
    workspaces: Vec<WorkspaceInfo>,
}

impl DesktopEnvironment {
    pub fn new(capabilities: DesktopCapabilities) -> Self {
        Self {
            capabilities,
            outputs: Vec::new(),
            windows: Vec::new(),
            workspaces: Vec::new(),
        }
    }

    /// The capability report of the provider feeding this environment.
    pub fn capabilities(&self) -> &DesktopCapabilities {
        &self.capabilities
    }

    /// Apply one event and report what changed.
    pub fn apply(&mut self, event: EnvironmentEvent) -> EnvironmentChange {
        match event {
            EnvironmentEvent::Snapshot(snap) => {
                let outputs = replaced(&self.outputs, &snap.outputs, |o| o.id.clone());
                let windows = replaced(&self.windows, &snap.windows, |w| w.id);
                let workspaces = replaced(&self.workspaces, &snap.workspaces, |w| w.id);
                let work_areas = outputs && work_areas_differ(&self.outputs, &snap.outputs);
                self.outputs = snap.outputs;
                self.windows = snap.windows;
                self.workspaces = snap.workspaces;
                EnvironmentChange {
                    outputs_changed: outputs,
                    work_areas_changed: work_areas,
                    windows_changed: windows,
                    workspaces_changed: workspaces,
                    ..EnvironmentChange::default()
                }
            }
            EnvironmentEvent::OutputsChanged(list) => {
                let outputs = replaced(&self.outputs, &list, |o| o.id.clone());
                let work_areas = outputs && work_areas_differ(&self.outputs, &list);
                self.outputs = list;
                EnvironmentChange {
                    outputs_changed: outputs,
                    work_areas_changed: work_areas,
                    ..EnvironmentChange::default()
                }
            }
            EnvironmentEvent::OutputAdded(output) | EnvironmentEvent::OutputChanged(output) => {
                self.upsert_output(output)
            }
            EnvironmentEvent::OutputRemoved { id } => {
                let before = self.outputs.len();
                self.outputs.retain(|o| o.id != id);
                EnvironmentChange {
                    outputs_changed: self.outputs.len() != before,
                    ..EnvironmentChange::default()
                }
            }
            EnvironmentEvent::WindowsChanged(list) => {
                let windows = replaced(&self.windows, &list, |w| w.id);
                self.windows = list;
                EnvironmentChange {
                    windows_changed: windows,
                    ..EnvironmentChange::default()
                }
            }
            EnvironmentEvent::WindowAdded(window) | EnvironmentEvent::WindowChanged(window) => {
                self.upsert_window(window)
            }
            EnvironmentEvent::WindowRemoved { id } => {
                let before = self.windows.len();
                self.windows.retain(|w| w.id != id);
                EnvironmentChange {
                    windows_changed: self.windows.len() != before,
                    ..EnvironmentChange::default()
                }
            }
            EnvironmentEvent::WindowFocused { id } => self.apply_focus(id),
            EnvironmentEvent::WorkspacesChanged(list) => {
                let workspaces = replaced(&self.workspaces, &list, |w| w.id);
                self.workspaces = list;
                EnvironmentChange {
                    workspaces_changed: workspaces,
                    ..EnvironmentChange::default()
                }
            }
            EnvironmentEvent::WorkspaceActivated { id, focused } => {
                self.apply_workspace_activated(id, focused)
            }
            EnvironmentEvent::WorkspaceActiveWindowChanged {
                workspace_id,
                active_window_id,
            } => {
                let mut change = EnvironmentChange::default();
                if let Some(ws) = self.workspaces.iter_mut().find(|w| w.id == workspace_id) {
                    if ws.active_window_id != active_window_id {
                        ws.active_window_id = active_window_id;
                        change.workspaces_changed = true;
                    }
                }
                change
            }
            EnvironmentEvent::ProviderLost => EnvironmentChange {
                provider_lost: true,
                ..EnvironmentChange::default()
            },
        }
    }

    fn upsert_output(&mut self, output: OutputInfo) -> EnvironmentChange {
        let mut change = EnvironmentChange::default();
        match self.outputs.iter_mut().find(|o| o.id == output.id) {
            Some(slot) => {
                if slot.work_area != output.work_area {
                    change.work_areas_changed = true;
                }
                if *slot != output {
                    *slot = output;
                    change.outputs_changed = true;
                }
            }
            None => {
                change.work_areas_changed = output.work_area.is_some();
                self.outputs.push(output);
                change.outputs_changed = true;
            }
        }
        change
    }

    fn upsert_window(&mut self, window: WindowInfo) -> EnvironmentChange {
        let changed = match self.windows.iter_mut().find(|w| w.id == window.id) {
            Some(slot) => {
                let changed = *slot != window;
                if changed {
                    *slot = window;
                }
                changed
            }
            None => {
                self.windows.push(window);
                true
            }
        };
        // Insert in id order so state stays deterministic regardless of
        // the order events arrive in.
        self.windows.sort_by_key(|w| w.id);
        EnvironmentChange {
            windows_changed: changed,
            ..EnvironmentChange::default()
        }
    }

    fn apply_focus(&mut self, id: Option<u64>) -> EnvironmentChange {
        let mut changed = false;
        for window in &mut self.windows {
            let focused = Some(window.id == id.unwrap_or(u64::MAX));
            if window.state.focused != focused {
                window.state.focused = focused;
                changed = true;
            }
        }
        EnvironmentChange {
            focus_changed: changed,
            ..EnvironmentChange::default()
        }
    }

    fn apply_workspace_activated(&mut self, id: u64, focused: Option<bool>) -> EnvironmentChange {
        let mut changed = false;
        // The activated workspace's output: other workspaces on the same
        // output lose their active state; a focused switch clears focus
        // everywhere else.
        let output = self
            .workspaces
            .iter()
            .find(|w| w.id == id)
            .and_then(|w| w.output.clone());
        for ws in &mut self.workspaces {
            if ws.id == id {
                if ws.active != Some(true) {
                    ws.active = Some(true);
                    changed = true;
                }
                if focused.is_some() && ws.focused != focused {
                    ws.focused = focused;
                    changed = true;
                }
            } else if output.as_deref() == ws.output.as_deref() && ws.active != Some(false) {
                ws.active = Some(false);
                changed = true;
            }
            if focused == Some(true) && ws.id != id && ws.focused == Some(true) {
                ws.focused = Some(false);
                changed = true;
            }
        }
        EnvironmentChange {
            workspaces_changed: changed,
            ..EnvironmentChange::default()
        }
    }

    // --- queries -------------------------------------------------------------

    pub fn outputs(&self) -> &[OutputInfo] {
        &self.outputs
    }

    pub fn windows(&self) -> &[WindowInfo] {
        &self.windows
    }

    pub fn workspaces(&self) -> &[WorkspaceInfo] {
        &self.workspaces
    }

    pub fn output(&self, id: &str) -> Option<&OutputInfo> {
        self.outputs.iter().find(|o| o.id == id)
    }

    pub fn window(&self, id: u64) -> Option<&WindowInfo> {
        self.windows.iter().find(|w| w.id == id)
    }

    pub fn focused_window(&self) -> Option<&WindowInfo> {
        self.windows.iter().find(|w| w.state.focused == Some(true))
    }

    /// A point-in-time copy of the whole state.
    pub fn snapshot(&self) -> DesktopSnapshot {
        DesktopSnapshot {
            outputs: self.outputs.clone(),
            windows: self.windows.clone(),
            workspaces: self.workspaces.clone(),
        }
    }

    /// The region-based spatial model of the current outputs.
    pub fn layout(&self) -> DesktopLayout {
        DesktopLayout::new(&self.outputs)
    }

    /// The placement rect for character-anchored logic on an output, in
    /// **output-local** logical coordinates: the work area when the
    /// compositor reports one, the full output otherwise.
    pub fn output_bounds(&self, output_id: &str) -> Option<Rect> {
        let output = self.output(output_id)?;
        if let Some(area) = output.work_area {
            return Some(area);
        }
        let (width, height) = output.logical_size?;
        Some(Rect {
            x: 0,
            y: 0,
            width: width as i32,
            height: height as i32,
        })
    }

    /// Windows whose reported geometry places them on the given output,
    /// paired with their geometry converted to **output-local** logical
    /// coordinates. Windows without geometry (the compositor does not
    /// expose one) are not listed — absence here is truthful, never a
    /// guess.
    ///
    /// A window belongs to the output containing its geometry center;
    /// windows straddling two outputs are reported on that side only.
    pub fn windows_on_output(&self, output_id: &str) -> Vec<(WindowInfo, WindowGeometry)> {
        let Some(output) = self.output(output_id) else {
            return Vec::new();
        };
        let (Some((ox, oy)), Some((ow, oh))) = (output.logical_position, output.logical_size)
        else {
            return Vec::new();
        };
        let (origin, extent) = ((ox as f64, oy as f64), (ow as f64, oh as f64));
        self.windows
            .iter()
            .filter_map(|window| {
                let global = window.geometry?;
                let center = (
                    global.x + global.width / 2.0,
                    global.y + global.height / 2.0,
                );
                let inside = center.0 >= origin.0
                    && center.0 < origin.0 + extent.0
                    && center.1 >= origin.1
                    && center.1 < origin.1 + extent.1;
                inside.then(|| {
                    (
                        window.clone(),
                        WindowGeometry {
                            x: global.x - origin.0,
                            y: global.y - origin.1,
                            ..global
                        },
                    )
                })
            })
            .collect()
    }
}

/// Whether two full-replacement lists describe different sets (id-keyed,
/// order-insensitive).
fn replaced<T: PartialEq, K: Ord>(old: &[T], new: &[T], key: impl Fn(&T) -> K + Copy) -> bool {
    if old.len() != new.len() {
        return true;
    }
    // Sort by key: a reordered set with identical content is no change
    // (providers may reorder between refreshes).
    let mut a: Vec<(K, &T)> = old.iter().map(|item| (key(item), item)).collect();
    let mut b: Vec<(K, &T)> = new.iter().map(|item| (key(item), item)).collect();
    a.sort_by(|x, y| x.0.cmp(&y.0));
    b.sort_by(|x, y| x.0.cmp(&y.0));
    a.iter()
        .zip(b.iter())
        .any(|((_, old_item), (_, new_item))| *old_item != *new_item)
}

/// Whether any output's work area differs between the two lists.
fn work_areas_differ(old: &[OutputInfo], new: &[OutputInfo]) -> bool {
    old.iter().any(|o| {
        new.iter()
            .find(|n| n.id == o.id)
            .is_some_and(|n| n.work_area != o.work_area)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{OutputInfo, WindowGeometry, WindowInfo, WindowState, WorkspaceInfo};

    fn output(id: &str, x: i32, y: i32, w: u32, h: u32) -> OutputInfo {
        OutputInfo {
            id: id.to_string(),
            name: id.to_string(),
            logical_position: Some((x, y)),
            logical_size: Some((w, h)),
            scale: Some(1.0),
            transform: None,
            physical: None,
            work_area: None,
            primary: None,
        }
    }

    fn window(id: u64, x: f64, y: f64, w: f64, h: f64) -> WindowInfo {
        WindowInfo {
            id,
            title: None,
            app_id: None,
            workspace_id: None,
            state: WindowState::default(),
            geometry: Some(WindowGeometry {
                x,
                y,
                width: w,
                height: h,
            }),
        }
    }

    fn no_geometry_window(id: u64) -> WindowInfo {
        WindowInfo {
            geometry: None,
            ..window(id, 0.0, 0.0, 1.0, 1.0)
        }
    }

    #[test]
    fn identical_snapshot_replacement_is_no_change() {
        let mut env = DesktopEnvironment::new(DesktopCapabilities::default());
        let snap = DesktopSnapshot {
            outputs: vec![output("eDP-1", 0, 0, 1536, 864)],
            windows: vec![window(1, 10.0, 10.0, 100.0, 50.0)],
            workspaces: Vec::new(),
        };
        let first = env.apply(EnvironmentEvent::Snapshot(snap.clone()));
        assert!(first.outputs_changed && first.windows_changed);

        let second = env.apply(EnvironmentEvent::Snapshot(snap));
        assert_eq!(second, EnvironmentChange::default());
    }

    #[test]
    fn reordered_snapshot_replacement_is_no_change() {
        let mut env = DesktopEnvironment::new(DesktopCapabilities::default());
        let snap = DesktopSnapshot {
            outputs: vec![output("a", 0, 0, 100, 100), output("b", 100, 0, 100, 100)],
            windows: Vec::new(),
            workspaces: Vec::new(),
        };
        let _ = env.apply(EnvironmentEvent::Snapshot(snap));
        let reordered = DesktopSnapshot {
            outputs: vec![output("b", 100, 0, 100, 100), output("a", 0, 0, 100, 100)],
            windows: Vec::new(),
            workspaces: Vec::new(),
        };
        let change = env.apply(EnvironmentEvent::Snapshot(reordered));
        assert_eq!(change, EnvironmentChange::default());
    }

    #[test]
    fn window_upsert_reports_only_real_changes() {
        let mut env = DesktopEnvironment::new(DesktopCapabilities::default());
        let change = env.apply(EnvironmentEvent::WindowAdded(window(
            7, 0.0, 0.0, 10.0, 10.0,
        )));
        assert!(change.windows_changed);

        let same = env.apply(EnvironmentEvent::WindowChanged(window(
            7, 0.0, 0.0, 10.0, 10.0,
        )));
        assert_eq!(same, EnvironmentChange::default());

        let moved = env.apply(EnvironmentEvent::WindowChanged(window(
            7, 5.0, 0.0, 10.0, 10.0,
        )));
        assert!(moved.windows_changed);
        assert_eq!(env.window(7).unwrap().geometry.unwrap().x, 5.0);
    }

    #[test]
    fn window_removal_and_missing_geometry_are_represented() {
        let mut env = DesktopEnvironment::new(DesktopCapabilities::default());
        let _ = env.apply(EnvironmentEvent::WindowAdded(no_geometry_window(3)));
        let listed = env.windows_on_output("eDP-1");
        assert!(listed.is_empty(), "no geometry → not listed, not guessed");

        let change = env.apply(EnvironmentEvent::WindowRemoved { id: 3 });
        assert!(change.windows_changed);
        let change = env.apply(EnvironmentEvent::WindowRemoved { id: 3 });
        assert_eq!(change, EnvironmentChange::default());
    }

    #[test]
    fn focus_event_updates_all_windows_atomically() {
        let mut env = DesktopEnvironment::new(DesktopCapabilities::default());
        let _ = env.apply(EnvironmentEvent::WindowsChanged(vec![
            window(1, 0.0, 0.0, 10.0, 10.0),
            window(2, 20.0, 0.0, 10.0, 10.0),
        ]));
        let change = env.apply(EnvironmentEvent::WindowFocused { id: Some(2) });
        assert!(change.focus_changed);
        assert_eq!(env.window(1).unwrap().state.focused, Some(false));
        assert_eq!(env.window(2).unwrap().state.focused, Some(true));
        assert_eq!(env.focused_window().unwrap().id, 2);

        let same = env.apply(EnvironmentEvent::WindowFocused { id: Some(2) });
        assert_eq!(same, EnvironmentChange::default());

        let none = env.apply(EnvironmentEvent::WindowFocused { id: None });
        assert!(none.focus_changed);
        assert_eq!(env.focused_window(), None);
    }

    #[test]
    fn workspace_activation_is_scoped_to_the_output() {
        let mut env = DesktopEnvironment::new(DesktopCapabilities::default());
        let ws = |id: u64, out: &str, active: bool| WorkspaceInfo {
            id,
            index: None,
            name: None,
            output: Some(out.to_string()),
            active: Some(active),
            focused: Some(false),
            urgent: None,
            active_window_id: None,
        };
        let _ = env.apply(EnvironmentEvent::WorkspacesChanged(vec![
            ws(1, "eDP-1", true),
            ws(2, "eDP-1", false),
            ws(3, "HDMI-A-1", true),
        ]));
        let change = env.apply(EnvironmentEvent::WorkspaceActivated {
            id: 2,
            focused: Some(true),
        });
        assert!(change.workspaces_changed);
        assert_eq!(env.workspaces()[0].active, Some(false)); // ws 1 demoted
        assert_eq!(env.workspaces()[1].active, Some(true)); // ws 2 promoted
        assert_eq!(env.workspaces()[1].focused, Some(true));
        assert_eq!(env.workspaces()[2].active, Some(true)); // other output untouched
        assert_eq!(env.workspaces()[2].focused, Some(false)); // focus moved away
    }

    #[test]
    fn work_area_changes_are_flagged_separately() {
        let mut env = DesktopEnvironment::new(DesktopCapabilities::default());
        let _ = env.apply(EnvironmentEvent::OutputAdded(output(
            "eDP-1", 0, 0, 1536, 864,
        )));
        let with_area = OutputInfo {
            work_area: Some(Rect {
                x: 0,
                y: 30,
                width: 1536,
                height: 834,
            }),
            ..output("eDP-1", 0, 0, 1536, 864)
        };
        let change = env.apply(EnvironmentEvent::OutputChanged(with_area));
        assert!(change.outputs_changed && change.work_areas_changed);
        assert_eq!(
            env.output_bounds("eDP-1"),
            Some(Rect {
                x: 0,
                y: 30,
                width: 1536,
                height: 834
            })
        );
    }

    #[test]
    fn output_bounds_fall_back_to_the_full_output() {
        let mut env = DesktopEnvironment::new(DesktopCapabilities::default());
        let _ = env.apply(EnvironmentEvent::OutputAdded(output(
            "eDP-1", -100, -50, 1536, 864,
        )));
        assert_eq!(
            env.output_bounds("eDP-1"),
            Some(Rect {
                x: 0,
                y: 0,
                width: 1536,
                height: 864
            })
        );
        assert_eq!(env.output_bounds("missing"), None);
    }

    #[test]
    fn windows_on_output_converts_to_local_coordinates() {
        let mut env = DesktopEnvironment::new(DesktopCapabilities::default());
        let _ = env.apply(EnvironmentEvent::OutputsChanged(vec![
            output("eDP-1", 0, 0, 1536, 864),
            output("HDMI-A-1", 1536, 0, 1920, 1080),
        ]));
        let _ = env.apply(EnvironmentEvent::WindowsChanged(vec![
            window(1, 100.0, 50.0, 200.0, 100.0),  // on eDP-1
            window(2, 1600.0, 10.0, 300.0, 200.0), // on HDMI-A-1
            window(3, 1500.0, 0.0, 72.0, 72.0),    // center at 1536 → right output
        ]));
        let local = env.windows_on_output("eDP-1");
        assert_eq!(local.len(), 1);
        assert_eq!(local[0].0.id, 1);
        assert_eq!(local[0].1.x, 100.0);
        assert_eq!(local[0].1.y, 50.0);

        let right = env.windows_on_output("HDMI-A-1");
        assert_eq!(right.len(), 2);
        assert_eq!(right[0].1.x, 64.0); // 1600 - 1536
        assert_eq!(right[1].0.id, 3);
        assert_eq!(right[1].1.x, -36.0); // straddles the boundary; center decided
    }

    #[test]
    fn provider_lost_is_reported_and_keeps_the_last_state() {
        let mut env = DesktopEnvironment::new(DesktopCapabilities::default());
        let _ = env.apply(EnvironmentEvent::OutputAdded(output(
            "eDP-1", 0, 0, 100, 100,
        )));
        let change = env.apply(EnvironmentEvent::ProviderLost);
        assert!(change.provider_lost);
        assert_eq!(env.outputs().len(), 1, "stale data is kept, not cleared");
    }

    #[test]
    fn capabilities_are_reported_verbatim() {
        let caps = DesktopCapabilities {
            windows: true,
            ..DesktopCapabilities::default()
        };
        let env = DesktopEnvironment::new(caps);
        assert!(env.capabilities().windows);
        assert!(!env.capabilities().work_areas);
    }
}
