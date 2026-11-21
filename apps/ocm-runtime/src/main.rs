//! # ocm-runtime
//!
//! OpenCuteMate runtime: CLI + TOML config, platform dispatch, renderer
//! selection and the process entry point.
//!
//! The behaviour lives in the platform-neutral [`app`] module (character,
//! interaction, scheduler, desktop awareness integration); the platform
//! drivers own the native artifacts and event loops:
//!
//! - `wayland.rs` — Wayland layer-shell + calloop + Niri IPC (Linux),
//! - `windows.rs` — Win32 overlay + message loop + Win32 hooks (Windows).
//!
//! Renderers are selected identically on both platforms through
//! [`app::create_renderer`].

use std::process::ExitCode;
use std::time::Duration;

use clap::{ArgAction, Parser};
use ocm_core::logging;
use ocm_core::{Config, Layer};

mod app;
mod desktop;

#[cfg(target_os = "windows")]
mod windows;

#[cfg(unix)]
mod wayland;

/// How often the runtime re-polls the platform's output set when the
/// provider does not push output events (Niri IPC: none; Windows: no
/// monitor-change push for plain clients). Each poll is a single cheap
/// query; the interval only trades output-change latency against wakeups.
pub(crate) const OUTPUT_REFRESH_INTERVAL: Duration = Duration::from_secs(30);

/// Platform-independent options handed from the CLI to a driver.
pub(crate) struct RunOptions {
    pub(crate) renderer: String,
    pub(crate) no_desktop: bool,
}

#[derive(Parser, Debug)]
#[command(
    name = "ocm-runtime",
    about = "OpenCuteMate desktop companion runtime (prototype cube character)",
    version
)]
struct Args {
    /// Path to a TOML configuration file.
    #[arg(long, value_name = "FILE")]
    config: Option<std::path::PathBuf>,

    /// Layer: "overlay" (default) or "top" (Wayland layer-shell only).
    #[arg(long, value_name = "LAYER")]
    layer: Option<String>,

    /// Restrict pointer input to the character hitbox (default).
    #[arg(long, action = ArgAction::SetTrue, conflicts_with = "no_click_through")]
    click_through: bool,

    /// Make the whole surface interactive (disable click-through).
    #[arg(long, action = ArgAction::SetTrue, conflicts_with = "click_through")]
    no_click_through: bool,

    /// Frames-per-second cap (compositor vsync still applies).
    #[arg(long, value_name = "FPS")]
    fps: Option<u32>,

    /// Target output by name (Wayland output name / Win32 device name;
    /// defaults to the platform's primary/first output).
    #[arg(long, value_name = "NAME")]
    output: Option<String>,

    /// Initial character position as "X,Y" in [0,1] (top-left origin).
    #[arg(long, value_name = "X,Y")]
    position: Option<String>,

    /// Character height as a fraction of the output height.
    #[arg(long, value_name = "FRAC")]
    size: Option<f32>,

    /// Enable continuous animation (bob + spin).
    #[arg(long, action = ArgAction::SetTrue, conflicts_with = "no_animate")]
    animate: bool,

    /// Disable continuous animation (default).
    #[arg(long, action = ArgAction::SetTrue, conflicts_with = "animate")]
    no_animate: bool,

    /// Enable character physics: the character falls to the floor on
    /// spawn and after drag releases (toss with the release velocity).
    #[arg(long, action = ArgAction::SetTrue, conflicts_with = "no_physics")]
    physics: bool,

    /// Disable character physics (default): the character rests where it
    /// is placed or dropped.
    #[arg(long, action = ArgAction::SetTrue, conflicts_with = "physics")]
    no_physics: bool,

    /// Disable vsync (use best-effort no-vsync present mode).
    #[arg(long, action = ArgAction::SetTrue, conflicts_with = "vsync")]
    no_vsync: bool,

    /// Force vsync (default).
    #[arg(long, action = ArgAction::SetTrue, conflicts_with = "no_vsync")]
    vsync: bool,

    /// Log level filter (trace, debug, info, warn, error). RUST_LOG wins.
    #[arg(long, default_value = "info")]
    log_level: String,

    /// Rendering backend: "wgpu" (default) or "bevy".
    #[arg(long, value_name = "NAME")]
    renderer: Option<String>,

    /// Disable desktop environment awareness.
    #[arg(long, action = ArgAction::SetTrue)]
    no_desktop: bool,
}

fn main() -> ExitCode {
    let args = Args::parse();
    logging::init(&args.log_level);

    // ---- Configuration -----------------------------------------------------
    let mut config = match &args.config {
        Some(path) => match Config::load(path) {
            Ok(cfg) => cfg,
            Err(e) => {
                eprintln!("failed to load config {path:?}: {e}");
                return ExitCode::FAILURE;
            }
        },
        None => Config::default(),
    };

    if let Some(layer) = &args.layer {
        config.window.layer = match layer.to_ascii_lowercase().as_str() {
            "overlay" => Layer::Overlay,
            "top" => Layer::Top,
            other => {
                eprintln!("unknown layer {other:?}; expected \"overlay\" or \"top\"");
                return ExitCode::FAILURE;
            }
        };
    }
    if args.click_through {
        config.window.click_through = true;
    }
    if args.no_click_through {
        config.window.click_through = false;
    }
    if let Some(fps) = args.fps {
        config.render.fps_limit = fps.max(1);
    }
    if let Some(output) = &args.output {
        config.window.output = output.clone();
    }
    if let Some(position) = &args.position {
        match parse_position(position) {
            Some((x, y)) => {
                config.character.position_x = x;
                config.character.position_y = y;
            }
            None => {
                eprintln!("invalid --position {position:?}; expected \"0.5,0.5\"");
                return ExitCode::FAILURE;
            }
        }
    }
    if let Some(size) = args.size {
        if !(0.02..=1.0).contains(&size) {
            eprintln!("--size must be within [0.02, 1.0]");
            return ExitCode::FAILURE;
        }
        config.character.size = size;
    }
    if args.animate {
        config.character.animate = true;
    }
    if args.no_animate {
        config.character.animate = false;
    }
    if args.physics {
        config.character.physics = true;
    }
    if args.no_physics {
        config.character.physics = false;
    }
    if args.no_vsync {
        config.render.vsync = false;
    }
    if args.vsync {
        config.render.vsync = true;
    }

    tracing::info!(?config, "starting OpenCuteMate runtime");

    let opts = RunOptions {
        renderer: args.renderer.clone().unwrap_or_else(|| "wgpu".to_string()),
        no_desktop: args.no_desktop,
    };

    #[cfg(target_os = "windows")]
    {
        windows::run(config, &opts)
    }

    #[cfg(unix)]
    {
        wayland::run(config, &opts)
    }

    #[cfg(not(any(unix, target_os = "windows")))]
    {
        let _ = (config, opts);
        eprintln!("this platform has no runtime backend yet");
        ExitCode::FAILURE
    }
}

fn parse_position(text: &str) -> Option<(f32, f32)> {
    let (x, y) = text.split_once(',')?;
    let x: f32 = x.trim().parse().ok()?;
    let y: f32 = y.trim().parse().ok()?;
    if !(0.0..=1.0).contains(&x) || !(0.0..=1.0).contains(&y) {
        return None;
    }
    Some((x, y))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_valid_positions() {
        assert_eq!(parse_position("0.5,0.5"), Some((0.5, 0.5)));
        assert_eq!(parse_position(" 0.2 , 0.8 "), Some((0.2, 0.8)));
        assert_eq!(parse_position("0,1"), Some((0.0, 1.0)));
    }

    #[test]
    fn rejects_out_of_range_or_malformed_input() {
        assert_eq!(parse_position("1.5,0.5"), None);
        assert_eq!(parse_position("0.5"), None);
        assert_eq!(parse_position("a,b"), None);
        assert_eq!(parse_position(""), None);
    }
}
