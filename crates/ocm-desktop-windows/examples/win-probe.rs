//! Windows desktop-awareness diagnostic tool.
//!
//! Builds **only on Windows** (`cargo run -p ocm-desktop-windows
//! --example win-probe`). It connects the provider, prints the full
//! snapshot, and then streams every neutral environmental event until
//! interrupted (Ctrl+C) — the tool for manual validation on a real
//! Windows machine.
//!
//! What to check (from `docs/desktop-awareness.md` §10):
//!
//! - monitors: one line per display with physical resolution, DPI
//!   scale, logical position/size, work area and primary flag;
//!   arrange monitors with gaps, negative offsets and mixed DPI and
//!   verify the numbers against the real layout;
//! - windows: title, app id, process id, geometry (DWM visible
//!   bounds), minimized/maximized/focused/visible flags; drag a
//!   window across monitors and verify geometry follows continuously;
//! - events: open/close/restore/minimize/maximize/focus windows and
//!   watch the event stream — no polling delay, no unrelated events;
//! - teardown: Ctrl+C exits cleanly (hook thread joined).

#[cfg(windows)]
fn main() {
    use std::time::{Duration, Instant};

    use ocm_desktop::{DesktopEnvironment, DesktopProvider, EnvironmentEvent};

    let mut provider =
        ocm_desktop_windows::WindowsProvider::connect().expect("connect to the Windows session");
    let caps = provider.capabilities();
    println!(
        "provider: {} (event_stream={} work_areas={} window_state={} fullscreen={})",
        "win32",
        caps.event_stream,
        caps.work_areas,
        caps.window_min_max_state,
        caps.window_fullscreen_state,
    );

    let snapshot = provider.snapshot().expect("initial snapshot");
    println!();
    println!("== monitors ==");
    for output in &snapshot.outputs {
        println!(
            "{:<14} logical={:?}x{:?} scale={:?} pos={:?} physical={:?} refresh={:?} mHz work_area={:?} primary={:?}",
            output.id,
            output.logical_size.map(|s| s.0),
            output.logical_size.map(|s| s.1),
            output.scale,
            output.logical_position,
            output.physical.map(|p| p.resolution),
            output.physical.and_then(|p| p.refresh_mhz),
            output.work_area,
            output.primary,
        );
    }
    println!();
    println!(
        "== windows ({} entries, filtered: invisible/cloaked/tool) ==",
        snapshot.windows.len()
    );
    for window in &snapshot.windows {
        print_window("window", window);
    }
    println!();
    println!("== event stream (Ctrl+C to exit) ==");
    let mut env = DesktopEnvironment::new(caps);
    let _ = env.apply(EnvironmentEvent::Snapshot(snapshot));

    let start = Instant::now();
    loop {
        match provider.poll_events() {
            Ok(events) => {
                for event in events {
                    let change = env.apply(event.clone());
                    if change.any() {
                        report(&event);
                    }
                }
            }
            Err(e) => {
                println!("provider lost: {e}");
                return;
            }
        }
        std::thread::sleep(Duration::from_millis(50));
        let _ = start;
    }
}

#[cfg(windows)]
fn report(event: &ocm_desktop::EnvironmentEvent) {
    use ocm_desktop::EnvironmentEvent;
    match event {
        EnvironmentEvent::Snapshot(_) => println!("snapshot (unexpected mid-run)"),
        EnvironmentEvent::OutputsChanged(list) => {
            println!();
            println!("-- outputs changed ({}):", list.len());
            for output in list {
                println!(
                    "{:<14} logical pos={:?} size={:?} scale={:?} work_area={:?} primary={:?}",
                    output.id,
                    output.logical_position,
                    output.logical_size,
                    output.scale,
                    output.work_area,
                    output.primary
                );
            }
        }
        EnvironmentEvent::OutputAdded(o) => println!("output-added id={}", o.id),
        EnvironmentEvent::OutputChanged(o) => println!("output-changed id={}", o.id),
        EnvironmentEvent::OutputRemoved { id } => println!("output-removed id={id}"),
        EnvironmentEvent::WindowsChanged(list) => {
            println!();
            println!("-- windows changed ({}):", list.len());
            for w in list {
                print_window("window", w);
            }
        }
        EnvironmentEvent::WindowAdded(w) => print_window("window-added", w),
        EnvironmentEvent::WindowChanged(w) => print_window("window-changed", w),
        EnvironmentEvent::WindowRemoved { id } => println!("window-removed id={id}"),
        EnvironmentEvent::WindowFocused { id } => {
            println!(
                "window-focused id={}",
                id.map(|i| i.to_string()).unwrap_or("none".into())
            )
        }
        EnvironmentEvent::WorkspacesChanged(list) => {
            println!("workspaces-changed count={}", list.len())
        }
        EnvironmentEvent::WorkspaceActivated { id, focused } => {
            println!("workspace-activated id={id} focused={focused:?}")
        }
        EnvironmentEvent::WorkspaceActiveWindowChanged {
            workspace_id,
            active_window_id,
        } => {
            println!("workspace-active-window workspace={workspace_id} window={active_window_id:?}")
        }
        EnvironmentEvent::ProviderLost => println!("provider-lost"),
    }
}

#[cfg(windows)]
fn print_window(verb: &str, w: &ocm_desktop::WindowInfo) {
    let geometry = w
        .geometry
        .map(|g| format!("{:.0},{:.0} {:.0}x{:.0}", g.x, g.y, g.width, g.height))
        .unwrap_or_else(|| "none".to_string());
    println!(
        "{verb:<14} id={:<8} app={:?} pid={:?} geo={geometry} focused={:?} minimized={:?} maximized={:?} visible={:?} title={:?}",
        w.id,
        w.app_id.as_deref().unwrap_or("?"),
        w.pid,
        w.state.focused,
        w.state.minimized,
        w.state.maximized,
        w.state.visible,
        w.title.as_deref().unwrap_or(""),
    );
}

#[cfg(not(windows))]
fn main() {
    eprintln!("win-probe builds and runs on Windows only:");
    eprintln!("  cargo run -p ocm-desktop-windows --example win-probe");
    eprintln!("(the provider itself cross-checks via x86_64-pc-windows-gnu;");
    eprintln!(" see tests/windows-crosscheck.sh)");
    std::process::exit(1);
}
