//! Windows overlay diagnostic probe (builds on Windows only).
//!
//! Verifies the platform half of the Windows runtime without any GPU
//! work: monitor geometry, window creation, transparency mode plumbing,
//! click-through hit-testing and the event loop. Run:
//!
//! ```sh
//! cargo run -p ocm-platform-windows --example win-overlay-probe
//! cargo run -p ocm-platform-windows --example win-overlay-probe -- --color-key
//! ```
//!
//! What to check:
//! - the printed monitor/rect/scale values against the real display;
//! - the window is borderless, transparent (per-pixel alpha needs a
//!   renderer; `--color-key` shows the binary fallback as a magenta
//!   desktop hole), never appears in the taskbar and never takes focus
//!   (click a window behind it, then check it stays focused);
//! - moving the pointer over the central square produces Enter/Motion
//!   events; moving outside it produces Leave and the events go to the
//!   window below (click-through);
//! - dragging with the left button keeps producing Motion events even
//!   outside the square (capture);
//! - closing the window (Alt+F4 is suppressed by design; use Ctrl+C)
//!   ends cleanly.

#[cfg(windows)]
fn main() {
    use std::time::{Duration, Instant};

    use ocm_platform::{OverlayParams, OverlaySurface, Platform, PlatformEvent};
    use ocm_platform_windows::WindowsPlatform;

    let color_key = std::env::args().any(|a| a == "--color-key");
    let mut platform = WindowsPlatform::connect().expect("connect to the Windows session");

    let mut params = OverlayParams {
        layer: ocm_core::Layer::Overlay,
        keyboard_interactivity: ocm_core::KeyboardInteractivity::None,
        output: String::new(),
        namespace: "opencutemate-diagnostic".to_string(),
    };
    if let Some(name) = std::env::args().nth(1).filter(|a| !a.starts_with("--")) {
        params.output = name;
    }

    let monitors = platform.monitors();
    println!("== monitors ({}) ==", monitors.len());
    for m in &monitors {
        println!(
            "{:<14} logical_pos={:?} logical_size={:?} scale={}",
            m.name, m.logical_position, m.logical_size, m.scale
        );
    }

    let overlay = platform.create_overlay(&params).expect("create overlay");
    let diag = overlay.diagnostics();
    println!();
    println!("== overlay ==");
    println!(
        "device={} physical={:?} logical={:?} scale={}",
        diag.device, diag.physical_rect, diag.logical_size, diag.scale
    );
    println!("target output: {:?}", overlay.target_output());
    println!("frame callbacks: {}", overlay.supports_frame_callbacks());

    if color_key {
        overlay.set_color_key((255, 0, 255));
        println!("color key applied: {:?}", overlay.diagnostics().color_key);
    }

    // A central square is the interactive region; everything else must be
    // click-through. This is exactly the API the runtime uses with the
    // character's hitboxes.
    let (w, h) = overlay.logical_size();
    let rect = ocm_platform::Rect {
        x: (w as i32 - 200) / 2,
        y: (h as i32 - 200) / 2,
        width: 200,
        height: 200,
    };
    overlay.set_input_region(Some(&[rect]));
    println!("interactive region: {rect:?} (logical); everything else click-through");

    let wake = platform.wake_handle().expect("wake handle");
    println!("== live event stream (Ctrl+C to exit) ==");
    let start = Instant::now();
    loop {
        // Beat once per second so the loop is observably alive.
        let _ = platform.set_deadline(Some(Duration::from_secs(1)));
        match platform.wait_and_dispatch(Some(&wake), |event| match event {
            PlatformEvent::Pointer(p) => println!(
                "[{:>6.2}s] pointer {:?} at ({:.1}, {:.1})",
                start.elapsed().as_secs_f32(),
                p.kind,
                p.x,
                p.y
            ),
            PlatformEvent::Configure { width, height } => {
                println!(
                    "[{:>6.2}s] configure {width}x{height}",
                    start.elapsed().as_secs_f32()
                )
            }
            PlatformEvent::ScaleChanged { scale } => {
                println!("[{:>6.2}s] scale {scale}", start.elapsed().as_secs_f32())
            }
            PlatformEvent::Frame => {}
            PlatformEvent::Closed => println!("closed"),
        }) {
            Ok(ocm_platform_windows::Wakeup::Quit) => break,
            Ok(ocm_platform_windows::Wakeup::Timer) => {
                println!("[{:>6.2}s] (idle tick)", start.elapsed().as_secs_f32())
            }
            Ok(_) => {}
            Err(e) => {
                eprintln!("platform error: {e}");
                break;
            }
        }
        let _ = &wake;
    }
    println!("probe finished");
}

#[cfg(not(windows))]
fn main() {
    eprintln!("win-overlay-probe builds and runs on Windows only.");
    std::process::exit(1);
}
