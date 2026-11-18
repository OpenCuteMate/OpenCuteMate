use clap::Parser;
use ocm_core::Config;
use ocm_platform::{OverlaySurface, OverlayWindow, Platform, Rect, SurfaceHandle};
use ocm_renderer::{SceneState, WgpuRenderer};
use std::time::{Duration, Instant};

#[derive(Parser, Debug)]
#[command(name = "ocm-runtime", about = "OpenCuteMate desktop companion runtime")]
struct Args {
    #[arg(long)]
    config: Option<std::path::PathBuf>,
    #[arg(long, default_value = "overlay")]
    layer: String,
    #[arg(long, default_value_t = true)]
    click_through: bool,
    #[arg(long, default_value_t = 60)]
    fps: u32,
    #[arg(long, default_value_t = false)]
    vsync: bool,
}

fn main() {
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .init();

    let args = Args::parse();

    let mut config = args.config.as_ref().and_then(|p| Config::load(p).ok()).unwrap_or_default();

    if args.layer == "top" {
        config.layer = ocm_core::Layer::Top;
    } else {
        config.layer = ocm_core::Layer::Overlay;
    }
    config.click_through = args.click_through;
    config.fps_cap = args.fps;
    config.vsync = args.vsync;

    tracing::info!("starting OpenCuteMate PoC with config: {:?}", config);

    let platform = ocm_platform_wayland::WaylandPlatform::new()
        .expect("failed to connect to Wayland");

    let overlay = platform
        .create_overlay(&config)
        .expect("failed to create overlay");

    let (w, h) = overlay.logical_size();
    let raw_window = overlay.raw_window_handle();
    let raw_display = overlay.raw_display_handle();

    let mut renderer = WgpuRenderer::new(raw_window, raw_display, w, h)
        .expect("failed to init wgpu renderer");

    let mut scene = SceneState {
        character_x: 0.0,
        character_y: 0.0,
        character_size: config.character_size,
        rotation: 0.0,
        dirty: true,
        animating: true,
    };

    let mut last_frame = Instant::now();
    let frame_interval = Duration::from_millis(1000 / config.fps_cap.max(1) as u64);

    let mut frame_count: u64 = 0;

    loop {
        let now = Instant::now();
        let elapsed = now.duration_since(last_frame);

        if elapsed >= frame_interval {
            last_frame = now;

            scene.rotation += 0.01;
            scene.dirty = true;

            if let Err(e) = renderer.render(&scene) {
                tracing::error!("render error: {e}");
                break;
            }

            frame_count += 1;
            if frame_count % 60 == 0 {
                tracing::info!("frame {}, rotation {:.3}", frame_count, scene.rotation);
            }
        } else {
            std::thread::sleep(Duration::from_millis(1));
        }
    }
}