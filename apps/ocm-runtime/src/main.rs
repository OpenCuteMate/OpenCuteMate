//! # ocm-runtime
//!
//! OpenCuteMate runtime: CLI + TOML config, platform init, renderer init,
//! the calloop main loop, the state-based render scheduler and the
//! explicit teardown ordering.
//!
//! Phase 3a wiring: platform events are translated by
//! [`ocm_character::InteractionSystem`] into semantic interaction events,
//! consumed by the [`ocm_character::Character`] aggregate; renderers draw
//! the character's model and pose. The input region follows a pointer-grab
//! policy: element rects while idle (click-through preserved), the whole
//! surface while dragging (a drag can never outrun the character), with
//! commits deduplicated.

use std::process::ExitCode;
use std::time::{Duration, Instant};

use calloop::{EventLoop, LoopHandle, RegistrationToken};
use clap::{ArgAction, Parser};
use ocm_character::{Character, DesktopBounds, ElementHitbox, InteractionEvent, InteractionSystem};
use ocm_core::logging;
use ocm_core::{Config, Layer, PlatformError};
use ocm_desktop::{DesktopEnvironment, EnvironmentChange, EnvironmentEvent};
use ocm_desktop_niri::NiriProvider;

mod desktop;
use desktop as desktop_seam;
use ocm_platform::{OverlaySurface, Platform, PlatformEvent, Rect};
use ocm_platform_wayland::WaylandPlatform;
use ocm_renderer::{FrameOutcome, RenderOpts, Renderer, SceneState};

/// Seconds of inactivity before the character falls asleep.
const SLEEP_AFTER: Duration = Duration::from_secs(60);

/// How often the runtime re-polls the compositor's output set.
///
/// The Niri event stream covers windows and workspaces but not outputs
/// (no output event exists in the niri 26.04 IPC), so outputs are the
/// one environmental family that needs polling. Each poll is a single
/// IPC socket connect (~50 µs of work, no process spawn); the interval
/// only trades output-change latency against wakeups.
const OUTPUT_REFRESH_INTERVAL: Duration = Duration::from_secs(30);

/// Frame pacing / scheduler state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RenderMode {
    /// Parked: no frame callback armed, nothing scheduled. Zero wakeups.
    Idle,
    /// Interaction-driven: render when dirty, follow with one frame callback.
    EventDriven,
    /// Continuous updates while the character needs frames (physics in
    /// flight, one-shot reactions, or the legacy `animate` mode), driven
    /// by frame callbacks + FPS cap.
    Animated,
}

/// What the compositor input region currently says.
#[derive(Debug, Clone, PartialEq)]
enum InputRegion {
    /// Not applied yet.
    Unset,
    /// Click-through: only the character's element rects.
    Rects(Vec<Rect>),
    /// Pointer grab: the whole surface is interactive (dragging).
    FullSurface,
}

/// The desktop-awareness side of the app: the neutral environment state
/// fed by a provider (Niri IPC today) plus the loop tokens it owns.
///
/// Everything here is additive — with awareness disabled or the provider
/// unavailable, the runtime behaves exactly as in Phase 3a.
struct DesktopState {
    environment: DesktopEnvironment,
    /// Output the character lives on (the platform's target output).
    target_output: String,
    /// Event-source token (to remove on `ProviderLost`).
    source: Option<RegistrationToken>,
    /// Output re-poll timer (providers without output events need it).
    output_refresh: Option<RegistrationToken>,
    /// One-shot window-overlap report dedup (count, under).
    last_window_report: (usize, usize),
}

struct App {
    overlay: ocm_platform_wayland::WaylandOverlay,
    renderer: Box<dyn Renderer>,
    config: Config,
    /// Character system: phase machine, physics, pose.
    character: Character,
    /// Mechanical pointer interpretation (hover/press/drag).
    interaction: InteractionSystem,
    /// Bounds of the output the character lives on (logical px).
    bounds: DesktopBounds,
    mode: RenderMode,
    dirty: bool,
    last_present: Instant,
    last_update: Instant,
    frame_interval: Duration,
    timer: Option<RegistrationToken>,
    sleep_timer: Option<RegistrationToken>,
    /// The input region last committed to the compositor (dedup).
    input_region: InputRegion,
    quit: bool,
    frames: u64,
    idle_logged: bool,
    loop_handle: LoopHandle<'static, App>,
    /// Desktop environment awareness (optional; `None` = disabled or
    /// provider unavailable).
    desktop: Option<DesktopState>,
}

impl App {
    fn scene(&self) -> SceneState {
        SceneState {
            position: self.character.position(),
            size_frac: self.character.size_frac(),
            hover: self.character.hovered_element(),
            pose: self.character.pose(),
        }
    }

    fn hitboxes(&self) -> Vec<ElementHitbox> {
        self.renderer.character_hitboxes(&self.scene())
    }

    /// The compositor input region for the current state: element rects
    /// while resting (click-through outside the character), the whole
    /// surface while dragging (the logical pointer grab).
    fn desired_region(&mut self) -> Option<Vec<Rect>> {
        if !self.config.window.click_through {
            return None; // whole surface interactive, always
        }
        if self.character.is_dragging() {
            return None; // pointer grab: never let a fast drag escape
        }
        Some(
            self.hitboxes()
                .into_iter()
                .map(|hitbox| hitbox.rect)
                .collect(),
        )
    }

    /// Commit the input region if it changed. Commits are compositor
    /// round-trips — unnecessary ones are avoided by the dedup state.
    fn apply_input_region(&mut self) {
        let region = match self.desired_region() {
            None => InputRegion::FullSurface,
            Some(rects) => InputRegion::Rects(rects),
        };
        if self.input_region == region {
            return;
        }
        match &region {
            InputRegion::FullSurface => self.overlay.set_input_region(None),
            InputRegion::Rects(rects) => {
                tracing::debug!(count = rects.len(), "setting input region");
                self.overlay.set_input_region(Some(rects));
            }
            InputRegion::Unset => unreachable!(),
        }
        self.input_region = region;
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

    /// Advance the character by the wall time since the last advance,
    /// then encode + present one frame and arm a frame callback.
    fn render_now(&mut self) {
        let now = Instant::now();
        let dt = now
            .saturating_duration_since(self.last_update)
            .as_secs_f32();
        self.last_update = now;
        let outcome = self.character.update(dt);
        if outcome.dirty {
            // Position may have moved (fall/slide): the input region must
            // keep covering the character. Deduplicated, so reaction-only
            // frames with an unchanged position commit nothing.
            self.apply_input_region();
        }
        self.apply_mode(outcome);

        let scene = self.scene();
        // Request the frame callback BEFORE presenting: a wl_surface.frame
        // request latches onto the next commit, and the present in
        // renderer.render() is that commit. The callback then fires when the
        // compositor has consumed this frame — the correct pacing signal.
        // (Requesting it after present would leave it pending forever.)
        self.overlay.request_frame();
        match self.renderer.render(&scene) {
            Ok(FrameOutcome {
                presented: true, ..
            }) => {
                self.frames += 1;
                self.last_present = Instant::now();
                self.dirty = false;
                self.idle_logged = false;
                if self.mode == RenderMode::Idle {
                    self.mode = RenderMode::EventDriven;
                }
                tracing::debug!(frame = self.frames, "frame presented");
            }
            Ok(FrameOutcome {
                presented: false,
                skipped: Some(reason),
            }) => {
                // Transient: retry shortly via a timer (a frame callback
                // may not fire because nothing was committed).
                tracing::debug!(reason, "frame skipped");
                self.schedule_timer(Duration::from_millis(4));
            }
            Ok(FrameOutcome {
                presented: false,
                skipped: None,
            }) => {
                self.schedule_timer(Duration::from_millis(4));
            }
            Err(e) => {
                tracing::error!("render error: {e}");
                self.quit = true;
            }
        }
    }

    /// Fold a character outcome into the scheduler mode and the sleep
    /// timer.
    fn apply_mode(&mut self, outcome: ocm_character::UpdateOutcome) {
        if outcome.needs_frames {
            if self.mode != RenderMode::Animated {
                tracing::debug!("scheduler: continuous frames (character active)");
            }
            self.mode = RenderMode::Animated;
            self.cancel_sleep_timer();
        } else if self.mode == RenderMode::Animated {
            // Continuous work finished: fall back to event-driven.
            self.mode = RenderMode::EventDriven;
            self.dirty = true; // one final settled frame
            tracing::debug!("scheduler: character settled");
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

    /// Arm the one-shot sleep timer (idling towards Sleeping).
    fn arm_sleep_timer(&mut self) {
        if self.sleep_timer.is_some() || self.mode != RenderMode::Idle {
            return;
        }
        match self.loop_handle.insert_source(
            calloop::timer::Timer::from_duration(SLEEP_AFTER),
            |_, _, app| {
                app.on_sleep_timeout();
                calloop::timer::TimeoutAction::Drop
            },
        ) {
            Ok(token) => self.sleep_timer = Some(token),
            Err(e) => tracing::error!("failed to schedule sleep timer: {e}"),
        }
    }

    fn cancel_sleep_timer(&mut self) {
        if let Some(token) = self.sleep_timer.take() {
            self.loop_handle.remove(token);
        }
    }

    fn on_sleep_timeout(&mut self) {
        self.sleep_timer = None;
        if self.character.try_sleep() {
            if self.mode == RenderMode::Idle {
                self.mode = RenderMode::EventDriven;
            }
            self.dirty = true;
            self.maybe_render();
        }
    }

    /// Decide the next step after a frame callback fired.
    fn on_frame(&mut self) {
        match self.mode {
            RenderMode::Animated => {
                if self.character.wants_frames() {
                    // Continuous character work in flight: the next
                    // render_now advances the character exactly once.
                    self.dirty = true;
                    self.maybe_render();
                } else {
                    // The character settled: one final settled frame, then
                    // fall back to event-driven parking.
                    self.mode = RenderMode::EventDriven;
                    self.dirty = true;
                    tracing::debug!("scheduler: character settled");
                    self.maybe_render();
                }
            }
            RenderMode::EventDriven => {
                if self.dirty {
                    self.maybe_render();
                } else {
                    // Nothing to draw: park. No further callbacks are armed
                    // and no timers are scheduled — the thread blocks in
                    // dispatch(None) until the next platform event.
                    self.mode = RenderMode::Idle;
                    self.arm_sleep_timer();
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
                self.bounds = DesktopBounds::new(width, height);
                let outcome = self.character.set_bounds(self.bounds);
                self.dirty = true;
                self.apply_input_region();
                self.apply_mode(outcome);
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
        // Only pointer events can wake a sleeping or idle character: the
        // sleep timer is cancelled until the next park.
        self.cancel_sleep_timer();
        let now = Instant::now();
        // Reaction clocks start at this event, not at the last rendered
        // frame (which may have been a parked-state render long ago).
        self.last_update = now;
        let hitboxes = self.hitboxes();
        let events = self.interaction.on_pointer(now, &pointer, &hitboxes);

        let mut needs_frames = false;
        for event in &events {
            // The runtime-level exit affordance (unchanged since Phase 2).
            if let InteractionEvent::Press {
                element,
                button: ocm_platform::PointerButton::Right,
            } = event
            {
                let model = self.character.model();
                tracing::info!(
                    element = %model.element(*element).name,
                    "right click on character; quitting"
                );
                self.quit = true;
                return;
            }

            let outcome = self.character.handle_interaction(now, event);
            self.dirty |= outcome.dirty;
            needs_frames |= outcome.needs_frames;
        }
        if self.dirty {
            // Position may have changed (drag move): the region follows
            // whenever the pointer grabbed or released.
            self.apply_input_region();
        }
        if needs_frames {
            // A reaction/physics just started while parked.
            if self.mode == RenderMode::Idle {
                self.mode = RenderMode::EventDriven;
            }
            self.dirty = true;
        }
        self.maybe_render();
    }

    // --- desktop environment awareness ------------------------------------

    /// Feed one environmental event into the environment and react to
    /// what actually changed. Window and workspace updates never touch
    /// the scheduler; only a real change to the character's output
    /// geometry can affect the character (and through it, rendering).
    fn on_environment_event(&mut self, event: EnvironmentEvent) {
        let Some(desktop) = self.desktop.as_mut() else {
            return;
        };
        let change = desktop.environment.apply(event);
        self.on_environment_change(change);
    }

    fn on_environment_change(&mut self, change: EnvironmentChange) {
        if change.provider_lost {
            self.on_provider_lost();
        }
        if change.outputs_changed {
            self.sync_bounds_with_environment();
        }
        if change.windows_changed {
            self.report_windows_near_character();
        }
        if change.workspaces_changed || change.focus_changed {
            tracing::debug!(?change, "desktop environment updated");
        }
    }

    /// The provider's subscription died: drop its loop sources and keep
    /// the last known environment (marked stale by the loss itself).
    fn on_provider_lost(&mut self) {
        let Some(desktop) = self.desktop.as_mut() else {
            return;
        };
        tracing::warn!("desktop environment provider lost; awareness paused (state kept)");
        if let Some(token) = desktop.source.take() {
            self.loop_handle.remove(token);
        }
        if let Some(token) = desktop.output_refresh.take() {
            self.loop_handle.remove(token);
        }
    }

    /// Reconcile the character's bounds with the environment's view of
    /// its output: the work area when the compositor reports one, the
    /// reported logical output otherwise. The layer-shell configure
    /// (Phase 2 behavior) stays authoritative for the surface itself;
    /// this only ever moves the character when the *work area* differs
    /// from what the surface configure said.
    fn sync_bounds_with_environment(&mut self) {
        let Some(desktop) = self.desktop.as_ref() else {
            return;
        };
        let env = &desktop.environment;
        let Some(output) = env.output(&desktop.target_output) else {
            return; // not known yet; the configure remains in charge
        };
        if let Some((w, h)) = output.logical_size {
            let (ow, oh) = self.overlay.logical_size();
            if (w, h) != (ow, oh) {
                tracing::debug!(
                    ipc = ?(w, h),
                    surface = ?(ow, oh),
                    "IPC output size differs from the surface configure"
                );
            }
        }
        let bounds = match output.work_area {
            Some(area) => DesktopBounds::new(area.width.max(1) as u32, area.height.max(1) as u32),
            None => match output.logical_size {
                Some((w, h)) => DesktopBounds::new(w.max(1), h.max(1)),
                None => return,
            },
        };
        if bounds != self.bounds {
            tracing::info!(
                output = %desktop.target_output,
                bounds = ?(bounds.width, bounds.height),
                "character bounds updated from the environment"
            );
            self.bounds = bounds;
            let outcome = self.character.set_bounds(bounds);
            self.dirty = true;
            self.apply_input_region();
            self.apply_mode(outcome);
            self.maybe_render();
        }
    }

    /// What the environment currently knows about windows around the
    /// character. This is the Phase 3b *integration boundary* for future
    /// window interactions: geometry is only reported for windows whose
    /// compositor provides it (niri: floating windows), and a report is
    /// logged only when the counts change — environmental events never
    /// schedule frames for this.
    fn report_windows_near_character(&mut self) {
        let Some(desktop) = self.desktop.as_ref() else {
            return;
        };
        let windows = desktop
            .environment
            .windows_on_output(&desktop.target_output);
        if windows.is_empty() {
            return;
        }
        let (width, height) = self.overlay.logical_size();
        let position = self.character.position();
        let anchor = (position.0 * width as f64, position.1 * height as f64);
        let under = windows
            .iter()
            .filter(|(_, geometry)| geometry.contains(anchor))
            .count();
        let report = (windows.len(), under);
        if report == desktop.last_window_report {
            return; // unchanged; no log spam while windows jiggle
        }
        let target = desktop.target_output.clone();
        if let Some(desktop) = self.desktop.as_mut() {
            desktop.last_window_report = report;
        }
        tracing::info!(
            output = %target,
            count = report.0,
            under_character = report.1,
            "windows near the character (geometry as reported by the compositor)"
        );
    }

    /// Schedule the output re-poll (the one environmental family the
    /// event stream does not cover). One-shot timers re-armed from their
    /// own callback keep the pattern used elsewhere in the runtime.
    fn arm_output_refresh(&mut self) {
        let Some(desktop) = self.desktop.as_mut() else {
            return;
        };
        if desktop.output_refresh.is_some() {
            return;
        }
        match self.loop_handle.insert_source(
            calloop::timer::Timer::from_duration(OUTPUT_REFRESH_INTERVAL),
            |_, _, app| {
                app.refresh_outputs();
                calloop::timer::TimeoutAction::Drop // refresh_outputs re-arms
            },
        ) {
            Ok(token) => {
                if let Some(desktop) = self.desktop.as_mut() {
                    desktop.output_refresh = Some(token);
                }
            }
            Err(e) => tracing::error!("failed to schedule the output re-poll: {e}"),
        }
    }

    fn refresh_outputs(&mut self) {
        let Some(desktop) = self.desktop.as_mut() else {
            return;
        };
        // The timer consumed itself (Drop action); it re-arms below.
        desktop.output_refresh = None;
        let change = match NiriProvider::fetch_outputs(None) {
            Ok(list) => desktop
                .environment
                .apply(EnvironmentEvent::OutputsChanged(list)),
            Err(e) => {
                tracing::warn!("output re-poll failed: {e}");
                desktop.environment.apply(EnvironmentEvent::ProviderLost)
            }
        };
        self.arm_output_refresh();
        self.on_environment_change(change);
    }
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

    /// Disable desktop environment awareness (default: on when a Niri
    /// IPC socket is available).
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

    // ---- Character ------------------------------------------------------------
    let bounds = DesktopBounds::new(overlay.logical_size().0, overlay.logical_size().1);
    let character = Character::new(&config.character, ocm_character::prototype_cube(), bounds);
    tracing::info!(
        phase = character.phase().name(),
        physics = config.character.physics,
        "character initialized"
    );

    // ---- Renderer -------------------------------------------------------------
    let render_opts = RenderOpts {
        vsync: config.render.vsync,
    };
    let backend = args.renderer.as_deref().unwrap_or("wgpu");
    let renderer: Box<dyn Renderer> = match backend {
        "wgpu" => {
            #[cfg(feature = "renderer-wgpu")]
            {
                Box::new(
                    match ocm_renderer::WgpuRenderer::new(
                        &overlay.native_handles(),
                        overlay.logical_size(),
                        overlay.buffer_size(),
                        &render_opts,
                        character.model(),
                    ) {
                        Ok(r) => r,
                        Err(e) => {
                            eprintln!("failed to initialize wgpu renderer: {e}");
                            // Drop the renderer-less app before the platform
                            // (teardown order).
                            drop(overlay);
                            drop(platform);
                            return ExitCode::FAILURE;
                        }
                    },
                )
            }
            #[cfg(not(feature = "renderer-wgpu"))]
            {
                eprintln!("this binary was built without the wgpu backend");
                drop(overlay);
                drop(platform);
                return ExitCode::FAILURE;
            }
        }
        "bevy" => {
            #[cfg(feature = "renderer-bevy")]
            {
                Box::new(
                    match ocm_renderer::BevyRenderer::new(
                        &overlay.native_handles(),
                        overlay.logical_size(),
                        overlay.buffer_size(),
                        &render_opts,
                        character.model(),
                    ) {
                        Ok(r) => r,
                        Err(e) => {
                            eprintln!("failed to initialize bevy renderer: {e}");
                            drop(overlay);
                            drop(platform);
                            return ExitCode::FAILURE;
                        }
                    },
                )
            }
            #[cfg(not(feature = "renderer-bevy"))]
            {
                eprintln!(
                    "this binary was built without the bevy backend \
                           (build with --features renderer-bevy)"
                );
                drop(overlay);
                drop(platform);
                return ExitCode::FAILURE;
            }
        }
        other => {
            eprintln!(
                "unknown renderer {other:?}; expected \"wgpu\" or \"bevy\" \
                 (built backends: wgpu={}, bevy={})",
                cfg!(feature = "renderer-wgpu"),
                cfg!(feature = "renderer-bevy")
            );
            drop(overlay);
            drop(platform);
            return ExitCode::FAILURE;
        }
    };
    tracing::info!(backend, "renderer initialized");

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

    let frame_interval = Duration::from_millis(1000 / config.render.fps_limit.clamp(1, 240) as u64);

    let mut app = App {
        overlay: overlay.clone(),
        renderer,
        config: config.clone(),
        character,
        interaction: InteractionSystem::new(),
        bounds,
        mode: if config.character.animate {
            RenderMode::Animated
        } else {
            RenderMode::EventDriven
        },
        dirty: true,
        last_present: Instant::now()
            .checked_sub(Duration::from_secs(1))
            .unwrap_or_else(Instant::now),
        last_update: Instant::now(),
        frame_interval,
        timer: None,
        sleep_timer: None,
        input_region: InputRegion::Unset,
        quit: false,
        frames: 0,
        idle_logged: false,
        loop_handle: handle.clone(),
        desktop: None,
    };

    // ---- Desktop environment awareness (optional) ------------------------------
    // Platform seam: apps/ocm-runtime/src/desktop.rs (niri on Linux).
    desktop_seam::attach(&mut app, &handle, &overlay, args.no_desktop);

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
    // 1. Drop the app (and with it the renderer: the GPU surface releases
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
