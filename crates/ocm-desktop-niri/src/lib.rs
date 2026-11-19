//! # ocm-desktop-niri
//!
//! Desktop environment provider for Niri, implementing the neutral
//! [`ocm_desktop::DesktopProvider`] contract over Niri's IPC socket.
//!
//! ## How it talks to Niri (verified against niri 26.04)
//!
//! The IPC is JSON over a Unix socket (`$NIRI_SOCKET`): one request line,
//! one reply line; after an `{"EventStream":null}` request the connection
//! turns into a one-way stream of JSON event lines, with the *full current
//! state* arriving up-front. This crate opens that socket itself — no
//! `niri msg` subprocesses are ever spawned.
//!
//! The event stream covers workspaces and windows (open/close/focus/
//! layout/urgency) but **not outputs** (verified: niri 26.04 has no output
//! event). Output changes are therefore detected by a one-shot re-poll
//! (`refresh_outputs` — one socket connect, no process) that the consumer
//! schedules; see `docs/desktop-awareness.md` §6.
//!
//! ## Truthfulness
//!
//! - Window pixel geometry is only reported for **floating** windows
//!   (niri fills `tile_pos_in_workspace_view` for those only; scrolling-
//!   layout windows get sizes but never positions — verified in niri's
//!   layout sources and against the live compositor).
//! - Minimized/maximized/fullscreen are not exposed by the IPC at all:
//!   they stay `None`.
//! - Work areas and primary-output status are not exposed: capabilities
//!   report them as unavailable.
//!
//! Unknown event-stream variants are skipped (niri adds variants across
//! releases); a parse failure never takes the consumer down — the source
//! reports [`EnvironmentEvent::ProviderLost`] instead.

mod convert;
#[cfg(test)]
mod fixtures;
mod ipc;
mod source;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use ocm_desktop::{
    DesktopCapabilities, DesktopProvider, DesktopSnapshot, EnvironmentEvent, OutputInfo,
    ProviderError,
};

use crate::convert::NiriEvent;
use crate::ipc::IpcConnection;

pub use source::{NiriSource, SourceError};

/// Bridge the IPC error type into the neutral provider error (the two
/// variants map 1:1).
fn ipc_error(error: ipc::IpcError) -> ProviderError {
    match error {
        ipc::IpcError::Io(e) => ProviderError::Io(e),
        ipc::IpcError::Protocol(s) => ProviderError::Protocol(s),
    }
}

/// Unwrap niri's reply envelope: a successful payload is either the
/// `Response` enum wrapper (`{"Outputs": {...}}`, one key) or a plain
/// `"Handled"` string.
fn unwrap_response(payload: serde_json::Value) -> Result<serde_json::Value, ProviderError> {
    match payload {
        serde_json::Value::Object(map) if map.len() == 1 => {
            let (_, inner) = map.into_iter().next().expect("checked len");
            Ok(inner)
        }
        other => Ok(other),
    }
}

/// Reply line after an event-stream request, before events start.
const EVENT_STREAM_REQUEST: &str = "{\"EventStream\":null}\n";

/// Environment provider for a running Niri compositor.
///
/// The provider is single-threaded (its event socket is drained by the
/// consumer's event loop). [`NiriSource`] is the calloop integration.
pub struct NiriProvider {
    socket: PathBuf,
    /// The persistent event-stream connection (non-blocking after
    /// [`NiriProvider::connect`]).
    stream: IpcConnection,
    /// Bytes read from the stream that do not yet end with a newline.
    line_buf: Vec<u8>,
    /// Last known full window records, keyed by id — the layout-change
    /// events only carry `(id, layout)` pairs, so full neutral
    /// `WindowChanged` events need this cache.
    windows: BTreeMap<u64, ocm_desktop::WindowInfo>,
}

impl NiriProvider {
    /// Connect to a running Niri instance. `socket: None` reads
    /// `$NIRI_SOCKET` (what `niri msg` uses).
    ///
    /// Returns [`ProviderError::Unavailable`] when there is no Niri to
    /// talk to; awareness is optional and callers treat that as "run
    /// without it".
    pub fn connect(socket: Option<&Path>) -> Result<Self, ProviderError> {
        let socket = socket
            .map(|p| p.to_path_buf())
            .or_else(|| std::env::var_os("NIRI_SOCKET").map(PathBuf::from))
            .ok_or(ProviderError::Unavailable)?;
        let mut stream = IpcConnection::connect(&socket).map_err(|_| ProviderError::Unavailable)?;

        // Turn the connection into a persistent event stream. Niri
        // answers once ("Handled") and then pushes events forever.
        stream
            .request(EVENT_STREAM_REQUEST.trim_end())
            .map_err(|e| ProviderError::Protocol(e.to_string()))?;
        stream.set_nonblocking().map_err(ProviderError::Io)?;

        tracing::debug!(socket = %socket.display(), "niri event stream connected");
        Ok(Self {
            socket,
            stream,
            line_buf: Vec::new(),
            windows: BTreeMap::new(),
        })
    }

    /// One-shot output fetch on a fresh connection — the event stream
    /// does not report output changes, so consumers schedule this
    /// (cheap: one socket connect, no process).
    pub fn fetch_outputs(socket: Option<&Path>) -> Result<Vec<OutputInfo>, ProviderError> {
        let socket = socket
            .map(|p| p.to_path_buf())
            .or_else(|| std::env::var_os("NIRI_SOCKET").map(PathBuf::from))
            .ok_or(ProviderError::Unavailable)?;
        let mut conn = IpcConnection::connect(&socket).map_err(|_| ProviderError::Unavailable)?;
        let reply = conn
            .request("{\"Outputs\":null}")
            .map_err(|e| ProviderError::Protocol(e.to_string()))?;
        let outputs: std::collections::HashMap<String, convert::NiriOutput> =
            serde_json::from_value(unwrap_response(reply)?)
                .map_err(|e| ProviderError::Protocol(e.to_string()))?;
        Ok(convert::outputs_to_info(outputs))
    }

    /// Map one parsed niri event into neutral events, maintaining the
    /// window cache. Exposed (crate-private) for fixture tests.
    pub(crate) fn map_event(&mut self, event: NiriEvent) -> Vec<EnvironmentEvent> {
        match event {
            NiriEvent::WorkspacesChanged(list) => vec![EnvironmentEvent::WorkspacesChanged(
                list.iter().map(convert::workspace_to_info).collect(),
            )],
            NiriEvent::WorkspaceActivated { id, focused } => {
                vec![EnvironmentEvent::WorkspaceActivated {
                    id,
                    focused: Some(focused),
                }]
            }
            NiriEvent::WorkspaceActiveWindowChanged {
                workspace_id,
                active_window_id,
            } => vec![EnvironmentEvent::WorkspaceActiveWindowChanged {
                workspace_id,
                active_window_id,
            }],
            NiriEvent::WindowsChanged(list) => {
                let windows: Vec<ocm_desktop::WindowInfo> =
                    list.into_iter().map(convert::window_to_info).collect();
                self.windows = windows.iter().map(|w| (w.id, w.clone())).collect();
                vec![EnvironmentEvent::WindowsChanged(windows)]
            }
            NiriEvent::WindowOpenedOrChanged(window) => {
                let info = convert::window_to_info(window);
                let known = self.windows.contains_key(&info.id);
                self.windows.insert(info.id, info.clone());
                vec![if known {
                    EnvironmentEvent::WindowChanged(info)
                } else {
                    EnvironmentEvent::WindowAdded(info)
                }]
            }
            NiriEvent::WindowClosed(id) => {
                self.windows.remove(&id);
                vec![EnvironmentEvent::WindowRemoved { id }]
            }
            NiriEvent::Ignored => Vec::new(),
            NiriEvent::WindowFocusChanged(id) => {
                vec![EnvironmentEvent::WindowFocused { id }]
            }
            NiriEvent::WindowLayoutsChanged(changes) => changes
                .into_iter()
                .filter_map(|(id, layout)| {
                    // Only windows we already know can be patched; a
                    // layout event for an unknown id would require a
                    // full re-fetch, and losing one cosmetic update is
                    // better than inventing state.
                    let base = self.windows.get(&id)?;
                    let patched = convert::apply_layout(base, &layout);
                    self.windows.insert(id, patched.clone());
                    Some(EnvironmentEvent::WindowChanged(patched))
                })
                .collect(),
        }
    }
}

impl DesktopProvider for NiriProvider {
    fn name(&self) -> &'static str {
        "niri-ipc"
    }

    fn capabilities(&self) -> DesktopCapabilities {
        niri_capabilities()
    }

    /// Blocking snapshot via three one-shot requests (each on its own
    /// connection, each self-consistent; the combination may lag reality
    /// by microseconds — the event stream's up-front state re-syncs
    /// anything that raced).
    fn snapshot(&mut self) -> Result<DesktopSnapshot, ProviderError> {
        let fetch = |request: &str| -> Result<serde_json::Value, ProviderError> {
            let mut conn =
                IpcConnection::connect(&self.socket).map_err(|_| ProviderError::Unavailable)?;
            conn.request(request)
                .map_err(|e| ProviderError::Protocol(e.to_string()))
        };

        let outputs: std::collections::HashMap<String, convert::NiriOutput> =
            serde_json::from_value(unwrap_response(fetch("{\"Outputs\":null}")?)?)
                .map_err(|e| ProviderError::Protocol(e.to_string()))?;
        let windows: Vec<convert::NiriWindow> =
            serde_json::from_value(unwrap_response(fetch("{\"Windows\":null}")?)?)
                .map_err(|e| ProviderError::Protocol(e.to_string()))?;
        let workspaces: Vec<convert::NiriWorkspace> =
            serde_json::from_value(unwrap_response(fetch("{\"Workspaces\":null}")?)?)
                .map_err(|e| ProviderError::Protocol(e.to_string()))?;

        let windows: Vec<ocm_desktop::WindowInfo> =
            windows.into_iter().map(convert::window_to_info).collect();
        self.windows = windows.iter().map(|w| (w.id, w.clone())).collect();

        Ok(DesktopSnapshot {
            outputs: convert::outputs_to_info(outputs),
            windows,
            workspaces: workspaces.iter().map(convert::workspace_to_info).collect(),
        })
    }

    /// Non-blocking drain of the event stream. Partial lines are kept
    /// across calls; malformed lines are logged and skipped (niri may
    /// add event variants this crate does not know).
    fn poll_events(&mut self) -> Result<Vec<EnvironmentEvent>, ProviderError> {
        self.stream
            .read_available(&mut self.line_buf)
            .map_err(ipc_error)?;
        let complete = self.stream.take_lines(&mut self.line_buf);
        let mut events = Vec::new();
        for line in complete {
            match convert::parse_event_line(&line) {
                Ok(NiriEvent::Ignored) => {}
                Ok(event) => events.extend(self.map_event(event)),
                Err(e) => tracing::debug!("skipping unparsable niri event line: {e}"),
            }
        }
        Ok(events)
    }

    fn refresh_outputs(&mut self) -> Result<Vec<OutputInfo>, ProviderError> {
        Self::fetch_outputs(Some(&self.socket))
    }
}

/// The verified capability set of the Niri 26.04 IPC.
///
/// Every `false` here was checked against niri's IPC surface (source and
/// live session); see `docs/desktop-awareness.md` §8 for the matrix.
pub fn niri_capabilities() -> DesktopCapabilities {
    DesktopCapabilities {
        outputs: true,
        output_logical_geometry: true,
        output_scale: true,
        output_mode: true,
        output_transform: true,
        work_areas: false,
        primary_output: false,
        // No output events in the event stream → consumers re-poll.
        output_events: false,

        windows: true,
        window_identity: true,
        window_workspace: true,
        window_focus: true,
        // Floating windows only; scrolling-layout windows report sizes
        // but no positions.
        window_geometry: true,
        window_geometry_all: false,
        // Visibility is derivable (workspace activeness) but not directly
        // reported; left None.
        window_visibility: false,
        // The IPC has no minimized/maximized/fullscreen fields at all.
        window_min_max_state: false,
        window_fullscreen_state: false,

        workspaces: true,
        workspace_events: true,
        focus_events: true,
        event_stream: true,
    }
}
