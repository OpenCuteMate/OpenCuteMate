//! # ocm-runtime
//!
//! OpenCuteMate PoC runtime: CLI + TOML config, platform init, renderer
//! init, the calloop main loop, the state-based render scheduler and the
//! explicit teardown ordering.

use std::process::ExitCode;
use std::time::{Duration, Instant};

use calloop::{EventLoop, LoopHandle, RegistrationToken};
use clap::{ArgAction, Parser};
use ocm_core::logging;
use ocm_core::{Config, Layer, PlatformError};
use ocm_platform::{OverlaySurface, Platform, PlatformEvent, PointerEventKind, Rect};
use ocm_platform_wayland::WaylandPlatform;
use ocm_renderer::{FrameOutcome, Hitbox, RenderOpts, SceneState, WgpuRenderer};

/// Frame pacing / scheduler state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RenderMode {
    /// Parked: no frame callback armed, nothing scheduled. Zero wakeups.
    Idle,
    /// Interaction-driven: render when dirty, follow with one frame callback.
    EventDriven,
    /// Continuous animation, driven by frame callbacks + FPS cap.
    Animated,
}

struct App {
    overlay: ocm_platform_wayland::WaylandOverlay,
    renderer: WgpuRenderer,
    config: Config,
    /// Character state (normalized position etc.).
    character: CharacterState,
    /// Whether the pointer is currently over the character hitbox.
    hover: bool,
    drag: Option<DragState>,
    mode: RenderMode,
    dirty: bool,
    last_present: Instant,
    frame_interval: Duration,
    timer: Option<RegistrationToken>,
    quit: bool,
    frames: u64,
    idle_logged: bool,
    loop_handle: LoopHandle<'static, App>,
}

struct CharacterState {
    position: (f64, f64),
    size_frac: f32,
    animate: bool,
    start: Instant,
}

struct DragState {
    /// Pointer offset (logical px) from the character center at grab time.
    grab_offset: (f64, f64),
}

impl App {
    fn scene(&self) -> SceneState {
        SceneState {
            position: self.character.position,
            size_frac: self.character.size_frac,
            hover: self.hover,
            animate: self.character.animate,
            time: self.character.start.elapsed().as_secs_f32(),
        }
    }

    fn hitbox(&self) -> Hitbox {
        self.renderer.character_hitbox(&self.scene())
    }

    /// Apply the input region according to click-through config and the
    /// current character placement. Commits immediately (see
    /// docs/architecture.md §11).
    fn apply_input_region(&mut self) {
        if !self.config.window.click_through {
            // Click-through disabled: the whole surface is interactive.
            self.overlay.set_input_region(None);
            return;
        }
        let hitbox = self.hitbox();
        let rect = Rect {
            x: (hitbox.center_x - hitbox.radius).floor() as i32,
            y: (hitbox.center_y - hitbox.radius).floor() as i32,
            width: (hitbox.radius * 2.0).ceil() as i32,
            height: (hitbox.radius * 2.0).ceil() as i32,
        };
        tracing::debug!(?rect, "setting input region");
        self.overlay.set_input_region(Some(&[rect]));
    }

    /// Render now if the FPS cap allows, otherwise defer with a timer.
    fn maybe_render(&mut self) {
        let elapsed = self.last_present.elapsed();
        if elapsed >= self.frame_interval {
            self.render_now();
        } else {
            self.schedule_timer(self.frame_interval - elapsed);
        }
    }

    /// Encode + present one frame, then arm a frame callback.
    fn render_now(&mut self) {
        let scene = self.scene();
        // Request the frame callback BEFORE presenting: a wl_surface.frame
        // request latches onto the next commit, and the present in
        // renderer.render() is that commit. The callback then fires when the
        // compositor has consumed this frame — the correct pacing signal.
        // (Requesting it after present would leave it pending forever.)
        self.overlay.request_frame();
        match self.renderer.render(&scene) {
            Ok(FrameOutcome { presented: true, .. }) => {
                self.frames += 1;
                self.last_present = Instant::now();
                self.dirty = false;
                self.idle_logged = false;
                if self.mode == RenderMode::Idle {
                    self.mode = RenderMode::EventDriven;
                }
                tracing::debug!(frame = self.frames, "frame presented");
            }
            Ok(FrameOutcome { presented: false, skipped: Some(reason) }) => {
                // Transient: retry shortly via a timer (a frame callback
                // may not fire because nothing was committed).
                tracing::debug!(reason, "frame skipped");
                self.schedule_timer(Duration::from_millis(4));
            }
            Ok(FrameOutcome { presented: false, skipped: None }) => {
                self.schedule_timer(Duration::from_millis(4));
            }
            Err(e) => {
                tracing::error!("render error: {e}");
                self.quit = true;
            }
        }
    }

    /// One-shot timer for FPS-cap deferral / retry.
    ///
    /// Deduplicated: while one timer is pending, later requests piggyback on
    /// it — its callback re-arms with the exact remaining delay if it fires
    /// early. This avoids remove/insert churn (which leaves calloop warning
    /// about already-queued events for removed sources).
    fn schedule_timer(&mut self, delay: Duration) {
        if self.timer.is_some() {
            return; // a timer is already pending; its callback re-arms
        }
        match self.loop_handle.insert_source(
            calloop::timer::Timer::from_duration(delay),
            |_, _, app| {
                app.on_timer();
                calloop::timer::TimeoutAction::Drop
            },
        ) {
            Ok(token) => self.timer = Some(token),
            Err(e) => tracing::error!("failed to schedule render timer: {e}"),
        }
    }

    fn on_timer(&mut self) {
        // The timer fired and is gone; consume it before any re-arms.
        self.timer = None;
        if self.dirty || self.mode == RenderMode::Animated {
            self.maybe_render();
        }
    }

    /// Decide the next step after a frame callback fired.
    fn on_frame(&mut self) {
        match self.mode {
            RenderMode::Animated => {
                self.dirty = true; // animation advances every frame
                self.maybe_render();
            }
            RenderMode::EventDriven => {
                if self.dirty {
                    self.maybe_render();
                } else {
                    // Nothing to draw: park. No further callbacks are armed
                    // and no timers are scheduled — the thread blocks in
                    // dispatch(None) until the next platform event.
                    self.mode = RenderMode::Idle;
                    if !self.idle_logged {
                        tracing::info!(frames = self.frames, "rendering parked (idle)");
                        self.idle_logged = true;
                    }
                }
            }
            RenderMode::Idle => {
                // Frame callback while idle should not happen (we never arm
                // one); ignore defensively.
                tracing::debug!("unexpected frame callback while idle");
            }
        }
    }

    fn on_platform_event(&mut self, event: PlatformEvent) {
        match event {
            PlatformEvent::Pointer(pointer) => self.on_pointer(pointer),
            PlatformEvent::Configure { width, height } => {
                let buffer = self.overlay.buffer_size();
                self.renderer.resize((width, height), buffer);
                self.dirty = true;
                self.apply_input_region();
                self.maybe_render();
            }
            PlatformEvent::ScaleChanged { scale: _ } => {
                // The overlay already applied the scale; resize buffers.
                let buffer = self.overlay.buffer_size();
                let logical = self.overlay.logical_size();
                self.renderer.resize(logical, buffer);
                self.dirty = true;
                self.apply_input_region();
                self.maybe_render();
            }
            PlatformEvent::Frame => self.on_frame(),
            PlatformEvent::Closed => {
                tracing::info!("overlay closed; quitting");
                self.quit = true;
            }
        }
    }

    fn on_pointer(&mut self, pointer: ocm_platform::PointerEvent) {
        match pointer.kind {
            PointerEventKind::Enter | PointerEventKind::Motion => {
                let hitbox = self.hitbox();
                let over = hitbox.contains(pointer.x, pointer.y);
                if over != self.hover {
                    self.hover = over;
                    self.dirty = true;
                    tracing::debug!(hover = over, "hover state changed");
                }
                if let Some(drag) = &self.drag {
                    // Dragging: move the character so that the grab offset
                    // stays under the pointer.
                    let (w, h) = self.overlay.logical_size();
                    let cx = (pointer.x - drag.grab_offset.0).clamp(0.0, w as f64);
                    let cy = (pointer.y - drag.grab_offset.1).clamp(0.0, h as f64);
                    self.character.position = (cx / w.max(1) as f64, cy / h.max(1) as f64);
                    self.dirty = true;
                    self.apply_input_region();
                }
                self.maybe_render();
            }
            PointerEventKind::Leave => {
                if self.hover {
                    self.hover = false;
                    self.dirty = true;
                }
                self.maybe_render();
            }
            PointerEventKind::Press { button } => {
                let hitbox = self.hitbox();
                match button {
                    ocm_platform::PointerButton::Left => {
                        if hitbox.contains(pointer.x, pointer.y) {
                            let (w, h) = self.overlay.logical_size();
                            self.drag = Some(DragState {
                                grab_offset: (
                                    pointer.x - self.character.position.0 * w as f64,
                                    pointer.y - self.character.position.1 * h as f64,
                                ),
                            });
                            tracing::info!(x = pointer.x, y = pointer.y, "drag started");
                        }
                    }
                    ocm_platform::PointerButton::Right => {
                        if hitbox.contains(pointer.x, pointer.y) {
                            tracing::info!("right click on character; quitting");
                            self.quit = true;
                        } else {
                            tracing::debug!("right click outside hitbox (passed through)");
                        }
                    }
                    _ => {}
                }
            }
            PointerEventKind::Release { button } => {
                if matches!(button, ocm_platform::PointerButton::Left) && self.drag.is_some() {
                    self.drag = None;
                    tracing::info!(?self.character.position, "drag ended");
                }
            }
        }
    }
}

#[derive(Parser, Debug)]
#[command(
    name = "ocm-runtime",
    about = "OpenCuteMate desktop companion runtime (PoC: procedural cube)",
    version
)]
struct Args {
    /// Path to a TOML configuration file.
    #[arg(long, value_name = "FILE")]
    config: Option<std::path::PathBuf>,

    /// Layer: "overlay" (default) or "top".
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

    /// Target output by name (defaults to the first output).
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

    /// Disable vsync (use best-effort no-vsync present mode).
    #[arg(long, action = ArgAction::SetTrue, conflicts_with = "vsync")]
    no_vsync: bool,

    /// Force vsync (default).
    #[arg(long, action = ArgAction::SetTrue, conflicts_with = "no_vsync")]
    vsync: bool,

    /// Log level filter (trace, debug, info, warn, error). RUST_LOG wins.
    #[arg(long, default_value = "info")]
    log_level: String,
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
    if args.no_vsync {
        config.render.vsync = false;
    }
    if args.vsync {
        config.render.vsync = true;
    }

    tracing::info!(?config, "starting OpenCuteMate PoC");

    // ---- Platform ------------------------------------------------------------
    let mut platform = match WaylandPlatform::connect() {
        Ok(p) => p,
        Err(e @ PlatformError::Connect(_)) => {
            eprintln!("{e}");
            eprintln!("hint: is a Wayland session running (WAYLAND_DISPLAY set)?");
            return ExitCode::FAILURE;
        }
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::FAILURE;
        }
    };

    let params = ocm_platform::OverlayParams {
        layer: config.window.layer,
        keyboard_interactivity: config.window.keyboard_interactivity,
        output: config.window.output.clone(),
        namespace: "opencutemate".to_string(),
    };
    let overlay = match platform.create_overlay(&params) {
        Ok(o) => o,
        Err(e) => {
            eprintln!("failed to create overlay: {e}");
            return ExitCode::FAILURE;
        }
    };

    // ---- Renderer -------------------------------------------------------------
    let render_opts = RenderOpts { vsync: config.render.vsync };
    let renderer = match WgpuRenderer::new(
        &overlay.native_handles(),
        overlay.logical_size(),
        overlay.buffer_size(),
        &render_opts,
    ) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("failed to initialize renderer: {e}");
            // Drop the renderer-less app before the platform (teardown order).
            drop(overlay);
            drop(platform);
            return ExitCode::FAILURE;
        }
    };

    // ---- Main loop ---------------------------------------------------------------
    let mut loop_ = match EventLoop::try_new() {
        Ok(l) => l,
        Err(e) => {
            eprintln!("failed to create event loop: {e}");
            drop(renderer);
            drop(overlay);
            drop(platform);
            return ExitCode::FAILURE;
        }
    };
    let handle: LoopHandle<'static, App> = loop_.handle();

    let frame_interval =
        Duration::from_millis(1000 / config.render.fps_limit.clamp(1, 240) as u64);

    let mut app = App {
        overlay: overlay.clone(),
        renderer,
        config: config.clone(),
        character: CharacterState {
            position: (
                config.character.position_x.clamp(0.0, 1.0) as f64,
                config.character.position_y.clamp(0.0, 1.0) as f64,
            ),
            size_frac: config.character.size,
            animate: config.character.animate,
            start: Instant::now(),
        },
        hover: false,
        drag: None,
        mode: if config.character.animate {
            RenderMode::Animated
        } else {
            RenderMode::EventDriven
        },
        dirty: true,
        last_present: Instant::now()
            .checked_sub(Duration::from_secs(1))
            .unwrap_or_else(Instant::now),
        frame_interval,
        timer: None,
        quit: false,
        frames: 0,
        idle_logged: false,
        loop_handle: handle.clone(),
    };

    // Input region for the initial placement + first render.
    app.apply_input_region();
    app.render_now();

    // Wayland event source.
    let source = match platform.into_source() {
        Ok(s) => s,
        Err(e) => {
            eprintln!("failed to prepare the Wayland event source: {e}");
            drop(app); // renderer before platform-owned state
            return ExitCode::FAILURE;
        }
    };
    if let Err(e) = handle.insert_source(source, |event, _, app| {
        app.on_platform_event(event);
    }) {
        eprintln!("failed to register the Wayland event source: {e}");
        drop(app);
        return ExitCode::FAILURE;
    }

    tracing::info!("event loop running");

    let dispatch_result = loop {
        if app.quit {
            break Ok(());
        }
        // None = block until a source is ready: idle means zero wakeups.
        match loop_.dispatch(None, &mut app) {
            Ok(()) => {}
            Err(e) => break Err(e),
        }
    };
    if let Err(ref e) = dispatch_result {
        tracing::error!("event loop error: {e}");
    }

    // ---- Teardown: documented destruction order -----------------------------
    // 1. Drop the app (and with it the wgpu renderer: the Surface releases
    //    its Vulkan swapchain *before* any Wayland object dies).
    tracing::info!("teardown: dropping renderer (wgpu surface) first");
    drop(app);
    // 2. Drop the event loop: the Wayland source, its queue and shared state
    //    (the wl_surface and its friends) die last, together with the
    //    connection.
    tracing::info!("teardown: dropping platform/wayland session");
    drop(loop_);
    // 3. The overlay handle is the last Rc into the shared state; dropping
    //    it here (after the loop) guarantees the Wayland objects were alive
    //    for the renderer's entire lifetime.
    drop(overlay);
    tracing::info!("teardown complete");

    if dispatch_result.is_err() {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
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
