//! Environment-state probe for automated tests and manual inspection.
//!
//! Connects to Niri's IPC, prints the initial snapshot as grep-able
//! lines, and (unless `--once`) keeps printing every neutral event the
//! compositor pushes. Used by `tests/niri.sh` to verify provider
//! discovery, event delivery and window geometry against the nested
//! compositor.
//!
//! ```sh
//! NIRI_SOCKET=… cargo run -p ocm-desktop-niri --example env-probe -- --once
//! NIRI_SOCKET=… cargo run -p ocm-desktop-niri --example env-probe          # stream
//! ```

use std::time::Duration;

use ocm_desktop::{DesktopEnvironment, DesktopProvider, EnvironmentEvent};

fn main() {
    let once = std::env::args().any(|a| a == "--once");
    let provider =
        ocm_desktop_niri::NiriProvider::connect(None).expect("connect to niri (set NIRI_SOCKET)");
    let caps = provider.capabilities();
    let mut env = DesktopEnvironment::new(caps);
    let mut provider = provider;

    let snapshot = provider.snapshot().expect("initial snapshot");
    for output in &snapshot.outputs {
        print!(
            "output name={} logical={:?} scale={:?} pos={:?} physical={:?} work_area={:?} primary={:?}",
            output.id,
            output.logical_size,
            output.scale,
            output.logical_position,
            output.physical.map(|p| p.resolution),
            output.work_area,
            output.primary,
        );
        println!();
    }
    for window in &snapshot.windows {
        print_window("window", window);
    }
    println!(
        "snapshot outputs={} windows={} workspaces={}",
        snapshot.outputs.len(),
        snapshot.windows.len(),
        snapshot.workspaces.len()
    );
    let change = env.apply(ocm_desktop::EnvironmentEvent::Snapshot(snapshot));
    assert!(!change.any(), "first snapshot must establish, not change");

    if once {
        return;
    }

    loop {
        match provider.poll_events() {
            Ok(events) => {
                for event in events {
                    report(&mut env, event);
                }
            }
            Err(e) => {
                println!("provider-lost {e}");
                return;
            }
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn report(env: &mut DesktopEnvironment, event: EnvironmentEvent) {
    match &event {
        EnvironmentEvent::Snapshot(_) => println!("event snapshot (unexpected mid-run)"),
        EnvironmentEvent::OutputsChanged(list) => {
            println!("event outputs-changed count={}", list.len())
        }
        EnvironmentEvent::OutputAdded(o) => println!("event output-added id={}", o.id),
        EnvironmentEvent::OutputChanged(o) => println!("event output-changed id={}", o.id),
        EnvironmentEvent::OutputRemoved { id } => println!("event output-removed id={id}"),
        EnvironmentEvent::WindowsChanged(list) => {
            println!("event windows-changed count={}", list.len())
        }
        EnvironmentEvent::WindowRemoved { id } => println!("event window-removed id={id}"),
        EnvironmentEvent::WindowFocused { id } => {
            println!(
                "event window-focused id={}",
                id.map(|i| i.to_string()).unwrap_or("none".into())
            )
        }
        EnvironmentEvent::WorkspaceActivated { id, focused } => {
            println!("event workspace-activated id={id} focused={focused:?}")
        }
        EnvironmentEvent::WorkspaceActiveWindowChanged {
            workspace_id,
            active_window_id,
        } => println!(
            "event workspace-active-window workspace={} window={active_window_id:?}",
            workspace_id
        ),
        EnvironmentEvent::ProviderLost => println!("event provider-lost"),
        EnvironmentEvent::WindowAdded(w) | EnvironmentEvent::WindowChanged(w) => {
            let verb = if matches!(event, EnvironmentEvent::WindowAdded(_)) {
                "window-added"
            } else {
                "window-changed"
            };
            print_window(verb, w);
        }
        EnvironmentEvent::WorkspacesChanged(list) => {
            println!("event workspaces-changed count={}", list.len())
        }
    }
    env.apply(event);
}

fn print_window(verb: &str, w: &ocm_desktop::WindowInfo) {
    let geometry = w
        .geometry
        .map(|g| format!("{:.0},{:.0} {}x{}", g.x, g.y, g.width, g.height))
        .unwrap_or_else(|| "none".to_string());
    println!(
        "{verb} id={} app={:?} floating={:?} focused={:?} workspace={:?} geometry={geometry}",
        w.id,
        w.app_id.as_deref().unwrap_or("?"),
        w.state.floating,
        w.state.focused,
        w.workspace_id,
    );
}
