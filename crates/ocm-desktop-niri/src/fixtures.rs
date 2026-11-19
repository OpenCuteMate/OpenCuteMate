//! Mocked (fixture-based) tests of the Niri IPC → neutral translation.
//!
//! The fixtures are **captured wire data** from a real Niri 26.04 session
//! plus hand-written variants for states that are hard to stage (closed
//! windows, focus changes). These tests never touch a compositor; the
//! live-compositor coverage is in `tests/live.rs` and clearly separated.

use std::collections::BTreeMap;
use std::os::unix::net::UnixStream;

use ocm_desktop::{EnvironmentEvent, WindowInfo};

use crate::convert::NiriEvent;
use crate::{ipc::IpcConnection, NiriProvider};

/// A provider whose mapping logic runs without any compositor (the
/// stream is a connected socket pair that nothing reads).
struct TestProvider {
    provider: NiriProvider,
}

impl TestProvider {
    fn new() -> Self {
        // The mapping path never touches the stream; a socket pair is
        // the cheapest valid connection value.
        let (stream, _sink) = UnixStream::pair().expect("socket pair");
        Self {
            provider: NiriProvider {
                socket: "/nonexistent".into(),
                stream: IpcConnection::from_stream(stream),
                line_buf: Vec::new(),
                windows: BTreeMap::new(),
            },
        }
    }

    /// Parse one captured wire line and map it, like `poll_events` does.
    fn feed(&mut self, line: &str) -> Vec<EnvironmentEvent> {
        match crate::convert::parse_event_line(line) {
            Ok(NiriEvent::Ignored) => Vec::new(),
            Ok(event) => self.provider.map_event(event),
            Err(_) => Vec::new(),
        }
    }

    /// Seed the window cache the way `snapshot()` does.
    fn seed_snapshot(&mut self, windows_json: &str) {
        let windows: Vec<crate::convert::NiriWindow> =
            serde_json::from_str(windows_json).expect("fixture windows");
        let windows: Vec<WindowInfo> = windows
            .into_iter()
            .map(crate::convert::window_to_info)
            .collect();
        self.provider.windows = windows.iter().map(|w| (w.id, w.clone())).collect();
    }

    fn snapshot_windows(&self) -> Vec<WindowInfo> {
        self.provider.windows.values().cloned().collect()
    }
}

// Captured from the live dev session (`niri msg --json event-stream`).
const WORKSPACES_CHANGED: &str = r#"{"WorkspacesChanged":{"workspaces":[{"id":1,"idx":1,"name":null,"output":"eDP-1","is_urgent":false,"is_active":false,"is_focused":false,"active_window_id":6},{"id":4,"idx":4,"name":null,"output":"eDP-1","is_urgent":false,"is_active":true,"is_focused":true,"active_window_id":null}]}}"#;

// Captured: two tiled windows on inactive workspaces — note
// `tile_pos_in_workspace_view` is null for both.
const WINDOWS_CHANGED: &str = r#"{"WindowsChanged":{"windows":[{"id":7,"title":"Editor","app_id":"zen","pid":5027,"workspace_id":2,"is_focused":false,"is_floating":false,"is_urgent":false,"layout":{"pos_in_scrolling_layout":[1,1],"tile_size":[1466.4,734.4],"window_size":[1466,734],"tile_pos_in_workspace_view":null,"window_offset_in_tile":[0.0,0.0]}},{"id":15,"title":"Terminal","app_id":"zed","pid":21686,"workspace_id":3,"is_focused":false,"is_floating":false,"is_urgent":false,"layout":{"pos_in_scrolling_layout":[1,1],"tile_size":[1536.0,864.0],"window_size":[1536,864],"tile_pos_in_workspace_view":null,"window_offset_in_tile":[0.0,0.0]}}]}}"#;

// Floating window (staged variant of the captured shape) — niri fills
// the tile position for floating windows only.
const WINDOW_OPENED_FLOATING: &str = r#"{"WindowOpenedOrChanged":{"window":{"id":21,"title":"Notes","app_id":"kitty","pid":42,"workspace_id":4,"is_focused":true,"is_floating":true,"is_urgent":false,"layout":{"pos_in_scrolling_layout":null,"tile_size":[426.0,886.0],"window_size":[426,886],"tile_pos_in_workspace_view":[66.0,32.0],"window_offset_in_tile":[0.0,0.0]}}}}"#;

const WINDOW_LAYOUTS_CHANGED: &str = r#"{"WindowLayoutsChanged":{"changes":[[15,{"pos_in_scrolling_layout":[1,1],"tile_size":[1200.0,864.0],"window_size":[1200,864],"tile_pos_in_workspace_view":null,"window_offset_in_tile":[0.0,0.0]}],[21,{"pos_in_scrolling_layout":null,"tile_size":[426.0,886.0],"window_size":[426,886],"tile_pos_in_workspace_view":[100.0,50.0],"window_offset_in_tile":[0.0,0.0]}]]}}"#;

const WINDOW_CLOSED: &str = r#"{"WindowClosed":{"id":21}}"#;
const WINDOW_FOCUS: &str = r#"{"WindowFocusChanged":{"id":15}}"#;
const WORKSPACE_ACTIVATED: &str = r#"{"WorkspaceActivated":{"id":2,"focused":true}}"#;
const WORKSPACE_ACTIVE_WINDOW: &str =
    r#"{"WorkspaceActiveWindowChanged":{"workspace_id":4,"active_window_id":7}}"#;

// Known niri events with no neutral representation (captured).
const IGNORED_EVENT: &str = r#"{"ConfigLoaded":{"failed":false}}"#;
const IGNORED_CASTS: &str = r#"{"CastsChanged":{"casts":[]}}"#;

#[test]
fn captured_workspaces_line_maps_to_neutral_replacement() {
    let mut provider = TestProvider::new();
    let events = provider.feed(WORKSPACES_CHANGED);
    assert_eq!(events.len(), 1);
    let EnvironmentEvent::WorkspacesChanged(list) = &events[0] else {
        panic!("wrong variant: {:?}", events[0]);
    };
    assert_eq!(list.len(), 2);
    let active = list.iter().find(|w| w.active == Some(true)).unwrap();
    assert_eq!(active.id, 4);
    assert_eq!(active.output.as_deref(), Some("eDP-1"));
    assert_eq!(active.focused, Some(true));
}

#[test]
fn tiled_windows_have_no_geometry_and_floating_do() {
    let mut provider = TestProvider::new();
    let events = provider.feed(WINDOWS_CHANGED);
    let EnvironmentEvent::WindowsChanged(windows) = &events[0] else {
        panic!("wrong variant");
    };
    for window in windows {
        assert!(
            window.geometry.is_none(),
            "tiled window must have no geometry (niri reports none)"
        );
        assert_eq!(window.state.floating, Some(false));
    }

    let events = provider.feed(WINDOW_OPENED_FLOATING);
    let EnvironmentEvent::WindowAdded(window) = &events[0] else {
        panic!("wrong variant: {:?}", events[0]);
    };
    let geometry = window.geometry.expect("floating window has geometry");
    assert_eq!((geometry.x, geometry.y), (66.0, 32.0));
    assert_eq!((geometry.width, geometry.height), (426.0, 886.0));
    assert_eq!(window.state.focused, Some(true));
    assert_eq!(window.state.floating, Some(true));
    // Minimized/maximized/fullscreen are not exposed by niri's IPC.
    assert_eq!(window.state.minimized, None);
    assert_eq!(window.state.maximized, None);
    assert_eq!(window.state.fullscreen, None);
}

#[test]
fn layout_changes_patch_the_cached_window_and_reemit_it() {
    let mut provider = TestProvider::new();
    let _ = provider.feed(WINDOWS_CHANGED);
    let _ = provider.feed(WINDOW_OPENED_FLOATING);

    let events = provider.feed(WINDOW_LAYOUTS_CHANGED);
    assert_eq!(events.len(), 2, "both changed windows are re-emitted");
    let EnvironmentEvent::WindowChanged(tiled) = &events[0] else {
        panic!("wrong variant: {:?}", events[0]);
    };
    // id 15 (tiled) changed size but has no position — still no geometry.
    assert_eq!(tiled.id, 15);
    assert_eq!(tiled.geometry, None);

    // id 21 had its tile position moved to (100, 50).
    let EnvironmentEvent::WindowChanged(window) = &events[1] else {
        panic!("wrong variant: {:?}", events[1]);
    };
    assert_eq!(window.id, 21);
    let geometry = window.geometry.unwrap();
    assert_eq!((geometry.x, geometry.y), (100.0, 50.0));
    assert_eq!(
        window.title.as_deref(),
        Some("Notes"),
        "cache preserved identity"
    );
}

#[test]
fn closed_and_focused_events_reach_the_environment() {
    let mut provider = TestProvider::new();
    let _ = provider.feed(WINDOWS_CHANGED);
    let _ = provider.feed(WINDOW_OPENED_FLOATING);

    let events = provider.feed(WINDOW_FOCUS);
    assert_eq!(
        events,
        vec![EnvironmentEvent::WindowFocused { id: Some(15) }]
    );

    let events = provider.feed(WINDOW_CLOSED);
    assert_eq!(events, vec![EnvironmentEvent::WindowRemoved { id: 21 }]);
    assert!(
        !provider.provider.windows.contains_key(&21),
        "cache drops closed windows"
    );
}

#[test]
fn workspace_events_map_verbatim() {
    let mut provider = TestProvider::new();
    assert_eq!(
        provider.feed(WORKSPACE_ACTIVATED),
        vec![EnvironmentEvent::WorkspaceActivated {
            id: 2,
            focused: Some(true)
        }]
    );
    assert_eq!(
        provider.feed(WORKSPACE_ACTIVE_WINDOW),
        vec![EnvironmentEvent::WorkspaceActiveWindowChanged {
            workspace_id: 4,
            active_window_id: Some(7)
        }]
    );
}

#[test]
fn unknown_and_unrepresented_variants_are_ignored() {
    let mut provider = TestProvider::new();
    assert!(provider.feed(IGNORED_EVENT).is_empty());
    assert!(provider.feed(IGNORED_CASTS).is_empty());
    // A future niri variant this crate cannot know yet.
    assert!(provider
        .feed(r#"{"SomeFutureEvent":{"whatever":1}}"#)
        .is_empty());
}

#[test]
fn malformed_lines_never_panic() {
    let mut provider = TestProvider::new();
    assert!(provider.feed("not json at all").is_empty());
    assert!(provider
        .feed(r#"{"WorkspacesChanged":{"workspaces":"wrong shape"}}"#)
        .is_empty());
    assert!(provider.feed("").is_empty());
}

#[test]
fn snapshot_seeded_cache_can_be_patched() {
    let mut provider = TestProvider::new();
    provider.seed_snapshot(
        r#"[{"id":15,"title":"Terminal","app_id":null,"pid":null,"workspace_id":null,"is_focused":false,"is_floating":false,"is_urgent":false,"layout":{"pos_in_scrolling_layout":null,"tile_size":[100.0,100.0],"window_size":[100,100],"tile_pos_in_workspace_view":null,"window_offset_in_tile":[0.0,0.0]}}]"#,
    );
    // A layout change for the seeded window must be patchable.
    let events = provider.feed(
        r#"{"WindowLayoutsChanged":{"changes":[[15,{"pos_in_scrolling_layout":null,"tile_size":[100.0,100.0],"window_size":[100,100],"tile_pos_in_workspace_view":[5.0,5.0],"window_offset_in_tile":[0.0,0.0]}]]}}"#,
    );
    let EnvironmentEvent::WindowChanged(window) = &events[0] else {
        panic!("wrong variant: {:?}", events[0]);
    };
    let geometry = window.geometry.unwrap();
    assert_eq!((geometry.x, geometry.y), (5.0, 5.0));
}

#[test]
fn window_order_is_stable_across_event_arrivals() {
    let mut provider = TestProvider::new();
    let _ = provider.feed(WINDOWS_CHANGED); // ids 7, 15
    let _ = provider.feed(WINDOW_OPENED_FLOATING); // id 21 arrives late
    let windows: Vec<u64> = provider.snapshot_windows().iter().map(|w| w.id).collect();
    assert_eq!(windows, vec![7, 15, 21], "id-ordered regardless of arrival");
}
