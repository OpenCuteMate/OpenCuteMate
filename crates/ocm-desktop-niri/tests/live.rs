//! Live integration tests against a real Niri compositor.
//!
//! These run only when `$NIRI_SOCKET` points at a reachable niri IPC
//! socket (the dev machine's session, or the nested compositor used by
//! `tests/niri.sh`). Without one, they print a note and pass — this
//! keeps `cargo test --workspace` green on any machine.
//!
//! All requests here are read-only (snapshot + a short event-stream
//! listen); nothing is spawned, focused or closed.

use std::time::{Duration, Instant};

use ocm_desktop::{DesktopProvider, EnvironmentEvent};

#[test]
fn live_snapshot_and_event_stream() {
    let Some(socket) = std::env::var_os("NIRI_SOCKET") else {
        eprintln!("live test skipped: NIRI_SOCKET not set (no niri session)");
        return;
    };

    let mut provider = match ocm_desktop_niri::NiriProvider::connect(Some(socket.as_ref())) {
        Ok(p) => p,
        Err(e) => {
            // A socket path may exist without a live compositor behind
            // it; that is the same "unavailable" contract the runtime
            // relies on. Missing wire data is a bug in this crate.
            eprintln!("live test skipped: provider connect failed: {e}");
            return;
        }
    };

    // --- snapshot ----------------------------------------------------------
    let snap = provider
        .snapshot()
        .expect("live niri must answer the snapshot requests");
    assert!(
        !snap.outputs.is_empty(),
        "a live compositor always has at least one output"
    );
    for output in &snap.outputs {
        assert!(!output.id.is_empty());
        // Niri always reports logical geometry for mapped outputs.
        assert!(output.logical_size.is_some(), "niri reports logical size");
        if let Some((w, h)) = output.logical_size {
            assert!(w > 0 && h > 0);
        }
        if let Some(scale) = output.scale {
            assert!(scale > 0.0, "scale must be positive");
        }
        // Truthful absences (verified against niri 26.04's IPC).
        assert_eq!(output.work_area, None, "niri exposes no work areas");
        assert_eq!(output.primary, None, "niri has no primary concept");
    }
    for window in &snap.windows {
        // Tiled windows (the common case) carry no pixel geometry; a
        // floating window here would be fine too — just assert the
        // invariant, never a guess.
        if let Some(geometry) = &window.geometry {
            assert!(geometry.width > 0.0 && geometry.height > 0.0);
        }
        assert_eq!(window.state.minimized, None);
        assert_eq!(window.state.maximized, None);
        assert_eq!(window.state.fullscreen, None);
    }

    // --- event stream: the up-front state must arrive without I/O waiting --
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut saw_workspaces = false;
    let mut saw_windows = false;
    while Instant::now() < deadline && !(saw_workspaces && saw_windows) {
        for event in provider.poll_events().expect("live event stream") {
            match event {
                EnvironmentEvent::WorkspacesChanged(list) if !list.is_empty() => {
                    saw_workspaces = true;
                }
                EnvironmentEvent::WindowsChanged(_) => saw_windows = true,
                _ => {}
            }
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(
        saw_workspaces,
        "event stream must deliver workspaces up-front"
    );
    assert!(saw_windows, "event stream must deliver windows up-front");
}

#[test]
fn live_refresh_outputs_matches_snapshot_outputs() {
    let Some(socket) = std::env::var_os("NIRI_SOCKET") else {
        eprintln!("live test skipped: NIRI_SOCKET not set (no niri session)");
        return;
    };
    let mut provider = match ocm_desktop_niri::NiriProvider::connect(Some(socket.as_ref())) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("live test skipped: provider connect failed: {e}");
            return;
        }
    };
    let snap = provider.snapshot().expect("snapshot");
    let refreshed = provider.refresh_outputs().expect("one-shot outputs fetch");
    assert_eq!(
        refreshed, snap.outputs,
        "re-poll and snapshot must agree on the output set"
    );
}

#[test]
fn live_unavailable_socket_is_reported_as_unavailable() {
    // A path that cannot exist: the provider must report Unavailable,
    // which is exactly the "run without awareness" contract.
    let result = ocm_desktop_niri::NiriProvider::connect(Some(std::path::Path::new(
        "/nonexistent/ocm-test-socket",
    )));
    assert!(
        matches!(result, Err(ocm_desktop::ProviderError::Unavailable)),
        "dead socket must map to Unavailable"
    );
}
