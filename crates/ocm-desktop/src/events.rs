//! Environmental events and the change summary [`DesktopEnvironment`]
//! produces when applying them.
//!
//! The event set is deliberately small: providers translate their
//! compositor's change notifications into these variants (or, when they
//! can only resynchronize, into the full-replacement variants). The
//! environment diffs full replacements so consumers see stable,
//! fine-grained [`EnvironmentChange`] flags regardless of provider
//! granularity.

use crate::model::{DesktopSnapshot, OutputInfo, WindowInfo, WorkspaceInfo};

/// One change in the desktop environment.
#[derive(Debug, Clone, PartialEq)]
pub enum EnvironmentEvent {
    /// Complete state replacement (initial snapshot, or a provider that
    /// can only resynchronize). The environment diffs this against its
    /// current state.
    Snapshot(DesktopSnapshot),
    /// Full output-set replacement (e.g. a re-poll result).
    OutputsChanged(Vec<OutputInfo>),
    /// A new output appeared (upsert semantics on id collision).
    OutputAdded(OutputInfo),
    /// An output changed (upsert by id).
    OutputChanged(OutputInfo),
    /// An output disappeared.
    OutputRemoved { id: String },
    /// Full window-set replacement.
    WindowsChanged(Vec<WindowInfo>),
    /// A window appeared.
    WindowAdded(WindowInfo),
    /// A window changed (geometry, title, state — upsert by id).
    WindowChanged(WindowInfo),
    /// A window closed.
    WindowRemoved { id: u64 },
    /// A different window (or none) is now focused. Providers deliver
    /// this as an id so the environment can update all windows' focus
    /// flags atomically.
    WindowFocused { id: Option<u64> },
    /// Full workspace-set replacement.
    WorkspacesChanged(Vec<WorkspaceInfo>),
    /// A workspace became active on its output. `focused` reports
    /// whether it also became the focused workspace (when known).
    WorkspaceActivated { id: u64, focused: Option<bool> },
    /// The active window on a workspace changed.
    WorkspaceActiveWindowChanged {
        workspace_id: u64,
        active_window_id: Option<u64>,
    },
    /// The provider's connection was lost; its data is now stale until
    /// it reconnects. Consumers decide what to keep.
    ProviderLost,
}

/// What changed when an event was applied. Consumers use the flags to
/// decide whether *their* state is affected — e.g. the runtime recomputes
/// character bounds only on `outputs_changed`, and never schedules a
/// render for pure window/workspace updates.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct EnvironmentChange {
    /// The set of outputs changed (added/removed/updated).
    pub outputs_changed: bool,
    /// Some output's work area changed (subset of `outputs_changed`).
    pub work_areas_changed: bool,
    /// The set or content of windows changed.
    pub windows_changed: bool,
    /// The set or content of workspaces changed.
    pub workspaces_changed: bool,
    /// Which window is focused changed.
    pub focus_changed: bool,
    /// The provider connection was lost.
    pub provider_lost: bool,
}

impl EnvironmentChange {
    /// Whether any part of the environment changed at all.
    pub fn any(&self) -> bool {
        *self != Self::default()
    }
}
