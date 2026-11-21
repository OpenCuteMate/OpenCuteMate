//! Platform-neutral application core.
//!
//! Everything that *is* OpenCuteMate's runtime behaviour lives here and
//! knows nothing about Wayland, calloop or Win32: the character, the
//! interaction system, the scheduler (idle / event-driven / animated),
//! the input-region policy, the desktop-environment integration and the
//! render loop.
//!
//! Platform drivers (`wayland.rs`, `windows.rs`) own the artifacts and
//! translate their native events into [`App`] calls. Scheduling is
//! expressed as **deadlines** ([`App::next_deadline`] /
//! [`App::on_deadline`]) so each driver arms its own wakeup mechanism:
//! calloop timers on Linux, a high-resolution waitable timer on Windows.
//! The zero-idle invariant is preserved by construction: when
//! `next_deadline()` is `None` and nothing is dirty, the driver blocks
//! without any timer armed.

use std::time::{Duration, Instant};

use ocm_character::{Character, DesktopBounds, ElementHitbox, InteractionEvent, InteractionSystem};
use ocm_core::Config;
use ocm_desktop::{DesktopEnvironment, EnvironmentChange, EnvironmentEvent};
use ocm_platform::{OverlaySurface, PlatformEvent, Rect};
use ocm_renderer::{FrameOutcome, RenderOpts, Renderer, SceneState};

/// Seconds of inactivity before the character falls asleep.
pub(crate) const SLEEP_AFTER: Duration = Duration::from_secs(60);

/// Frame pacing / scheduler state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RenderMode {
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
pub(crate) enum InputRegion {
    /// Not applied yet.
    Unset,
    /// Click-through: only the character's element rects.
    Rects(Vec<Rect>),
    /// Pointer grab: the whole surface is interactive (dragging).
    FullSurface,
}

/// The runtime's view of desktop awareness: the neutral environment plus
/// the scheduler-facing deadlines. The provider itself is owned by the
/// platform driver (its transport is platform-specific).
pub(crate) struct DesktopState {
    pub(crate) environment: DesktopEnvironment,
    /// Output the character lives on (the platform's target output).
    pub(crate) target_output: String,
    /// When to ask the driver for a one-shot environment refresh.
    pub(crate) refresh_deadline: Option<Instant>,
    /// One-shot window-overlap report dedup (count, under).
    pub(crate) last_window_report: (usize, usize),
}

pub(crate) struct App {
    pub(crate) overlay: Box<dyn OverlaySurface>,
    pub(crate) renderer: Box<dyn Renderer>,
    pub(crate) config: Config,
    /// Character system: phase machine, physics, pose.
    pub(crate) character: Character,
    /// Mechanical pointer interpretation (hover/press/drag).
    pub(crate) interaction: InteractionSystem,
    /// Bounds of the output the character lives on (logical px).
    pub(crate) bounds: DesktopBounds,
    pub(crate) mode: RenderMode,
    pub(crate) dirty: bool,
    pub(crate) last_present: Instant,
    pub(crate) last_update: Instant,
    pub(crate) frame_interval: Duration,
    /// One-shot frame deadline (FPS-cap deferral / next frame opportunity
    /// on platforms without compositor frame callbacks).
    render_deadline: Option<Instant>,
    /// One-shot sleep deadline (idling towards `Sleeping`).
    sleep_deadline: Option<Instant>,
    /// The input region last committed to the compositor (dedup).
    pub(crate) input_region: InputRegion,
    pub(crate) quit: bool,
    pub(crate) frames: u64,
    idle_logged: bool,
    /// Desktop environment awareness (optional; `None` = disabled or
    /// provider unavailable).
    pub(crate) desktop: Option<DesktopState>,
    /// The scheduler wants the driver to fetch a fresh environment
    /// snapshot (output re-poll).
    desktop_refresh_request: bool,
    /// The provider is gone; the driver should stop pumping it.
    desktop_stop_request: bool,
}

impl App {
    pub(crate) fn new(
        overlay: Box<dyn OverlaySurface>,
        renderer: Box<dyn Renderer>,
        config: Config,
        character: Character,
        bounds: DesktopBounds,
    ) -> Self {
        let frame_interval =
            Duration::from_millis(1000 / config.render.fps_limit.clamp(1, 240) as u64);
        Self {
            overlay,
            renderer,
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
            render_deadline: None,
            sleep_deadline: None,
            input_region: InputRegion::Unset,
            quit: false,
            frames: 0,
            idle_logged: false,
            desktop: None,
            desktop_refresh_request: false,
            desktop_stop_request: false,
            config,
        }
    }

    /// Initial commit + first render (the drivers call this once after
    /// construction, before entering their loop).
    pub(crate) fn start(&mut self) {
        self.apply_input_region();
        self.render_now();
    }

    pub(crate) fn scene(&self) -> SceneState {
        SceneState {
            position: self.character.position(),
            size_frac: self.character.size_frac(),
            hover: self.character.hovered_element(),
            pose: self.character.pose(),
        }
    }

    pub(crate) fn hitboxes(&self) -> Vec<ElementHitbox> {
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

    /// Commit the input region if it changed. On Wayland these are
    /// compositor round-trips; on Windows they are the hit-test
    /// rectangles. Either way unnecessary updates are avoided by the
    /// dedup state.
    pub(crate) fn apply_input_region(&mut self) {
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

    // --- deadlines ---------------------------------------------------------

    /// The earliest instant the driver must wake this app for, or `None`
    /// when it is parked (no timer armed = zero wakeups).
    pub(crate) fn next_deadline(&self) -> Option<Instant> {
        let refresh = self.desktop.as_ref().and_then(|d| d.refresh_deadline);
        [self.render_deadline, self.sleep_deadline, refresh]
            .into_iter()
            .flatten()
            .min()
    }

    /// Fire every deadline that is due at `now`.
    pub(crate) fn on_deadline(&mut self, now: Instant) {
        if let Some(at) = self.render_deadline {
            if now >= at {
                self.render_deadline = None;
                self.on_frame();
            }
        }
        if let Some(at) = self.sleep_deadline {
            if now >= at {
                self.sleep_deadline = None;
                self.on_sleep_timeout();
            }
        }
        let refresh_due = self
            .desktop
            .as_ref()
            .and_then(|d| d.refresh_deadline)
            .is_some_and(|at| now >= at);
        if refresh_due {
            if let Some(desktop) = self.desktop.as_mut() {
                desktop.refresh_deadline = None;
            }
            self.desktop_refresh_request = true;
        }
    }

    /// One-shot frame deadline, deduplicated: while one is pending, later
    /// requests piggyback on it (its fire re-evaluates everything).
    fn schedule_frame(&mut self, delay: Duration) {
        if self.render_deadline.is_none() {
            self.render_deadline = Some(Instant::now() + delay);
        }
    }

    /// Render now if the FPS cap allows, otherwise defer with a deadline.
    fn maybe_render(&mut self) {
        let elapsed = self.last_present.elapsed();
        if elapsed >= self.frame_interval {
            self.render_now();
        } else {
            self.schedule_frame(self.frame_interval - elapsed);
        }
    }

    /// Advance the character by the wall time since the last advance,
    /// then encode + present one frame and arm the next opportunity.
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
        // Compositor-paced platforms: request the frame callback BEFORE
        // presenting (it latches onto the present's commit; the callback
        // then fires when the compositor consumed the frame). Timer-paced
        // platforms have no callback — the scheduler arms the next
        // opportunity itself after the present.
        if self.overlay.supports_frame_callbacks() {
            self.overlay.request_frame();
        }
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
                if !self.overlay.supports_frame_callbacks() {
                    // Ask for the next opportunity: a full frame interval
                    // while animating, immediately otherwise (one more
                    // decision pass parks the app when settled).
                    let delay = if self.mode == RenderMode::Animated {
                        self.frame_interval
                    } else {
                        Duration::from_millis(1)
                    };
                    self.schedule_frame(delay);
                }
                tracing::debug!(frame = self.frames, "frame presented");
            }
            Ok(FrameOutcome {
                presented: false,
                skipped: Some(reason),
            }) => {
                // Transient: retry shortly (a frame callback may not fire
                // because nothing was committed).
                tracing::debug!(reason, "frame skipped");
                self.schedule_frame(Duration::from_millis(4));
            }
            Ok(FrameOutcome {
                presented: false,
                skipped: None,
            }) => {
                self.schedule_frame(Duration::from_millis(4));
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

    /// Arm the one-shot sleep deadline (idling towards Sleeping).
    fn arm_sleep_timer(&mut self) {
        if self.sleep_deadline.is_some() || self.mode != RenderMode::Idle {
            return;
        }
        self.sleep_deadline = Some(Instant::now() + SLEEP_AFTER);
    }

    fn cancel_sleep_timer(&mut self) {
        self.sleep_deadline = None;
    }

    fn on_sleep_timeout(&mut self) {
        if self.character.try_sleep() {
            if self.mode == RenderMode::Idle {
                self.mode = RenderMode::EventDriven;
            }
            self.dirty = true;
            self.maybe_render();
        }
    }

    /// Decide the next step after a frame opportunity fired (compositor
    /// frame callback on Wayland, scheduler deadline on Windows).
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
                    // and no deadlines are scheduled — the driver blocks
                    // until the next platform event.
                    self.mode = RenderMode::Idle;
                    self.arm_sleep_timer();
                    if !self.idle_logged {
                        tracing::info!(frames = self.frames, "rendering parked (idle)");
                        self.idle_logged = true;
                    }
                }
            }
            RenderMode::Idle => {
                // A frame opportunity while idle should not happen (none is
                // armed); ignore defensively.
                tracing::debug!("unexpected frame callback while idle");
            }
        }
    }

    pub(crate) fn on_platform_event(&mut self, event: PlatformEvent) {
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
    pub(crate) fn on_environment_event(&mut self, event: EnvironmentEvent) {
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

    /// The provider's subscription died: ask the driver to stop pumping
    /// it, keep the last known environment (marked stale by the loss).
    fn on_provider_lost(&mut self) {
        if self.desktop.is_none() {
            return;
        }
        tracing::warn!("desktop environment provider lost; awareness paused (state kept)");
        self.desktop_stop_request = true;
    }

    /// Whether the driver should stop the desktop provider (once).
    pub(crate) fn take_desktop_stop_request(&mut self) -> bool {
        std::mem::take(&mut self.desktop_stop_request)
    }

    /// Whether the driver should fetch a fresh output set (once).
    pub(crate) fn take_desktop_refresh_request(&mut self) -> bool {
        std::mem::take(&mut self.desktop_refresh_request)
    }

    /// Reconcile the character's bounds with the environment's view of
    /// its output: the work area when the compositor reports one, the
    /// reported logical output otherwise. The platform configure (Phase 2
    /// behavior) stays authoritative for the surface itself; this only
    /// ever moves the character when the *work area* differs from what
    /// the surface configure said.
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
                    provider = ?(w, h),
                    surface = ?(ow, oh),
                    "provider output size differs from the surface configure"
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
    /// platform provides it, and a report is logged only when the counts
    /// change — environmental events never schedule frames for this.
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
            "windows near the character (geometry as reported by the platform)"
        );
    }

    /// Arm the periodic output refresh (the one environmental family not
    /// every provider pushes).
    pub(crate) fn arm_output_refresh(&mut self) {
        let Some(desktop) = self.desktop.as_mut() else {
            return;
        };
        if desktop.refresh_deadline.is_none() {
            desktop.refresh_deadline = Some(Instant::now() + crate::OUTPUT_REFRESH_INTERVAL);
        }
    }
}

/// Create the renderer selected by `backend` against the overlay.
///
/// Shared by both platform drivers so renderer selection (and its
/// feature-gated error messages) cannot drift between platforms.
pub(crate) fn create_renderer(
    backend: &str,
    overlay: &dyn OverlaySurface,
    model: &ocm_character::CharacterModel,
    opts: &RenderOpts,
) -> Result<Box<dyn Renderer>, String> {
    match backend {
        "wgpu" => {
            #[cfg(feature = "renderer-wgpu")]
            {
                ocm_renderer::WgpuRenderer::new(
                    &overlay.native_handles(),
                    overlay.logical_size(),
                    overlay.buffer_size(),
                    opts,
                    model,
                )
                .map(|r| Box::new(r) as Box<dyn Renderer>)
                .map_err(|e| format!("failed to initialize wgpu renderer: {e}"))
            }
            #[cfg(not(feature = "renderer-wgpu"))]
            {
                Err("this binary was built without the wgpu backend".into())
            }
        }
        "bevy" => {
            #[cfg(feature = "renderer-bevy")]
            {
                ocm_renderer::BevyRenderer::new(
                    &overlay.native_handles(),
                    overlay.logical_size(),
                    overlay.buffer_size(),
                    opts,
                    model,
                )
                .map(|r| Box::new(r) as Box<dyn Renderer>)
                .map_err(|e| format!("failed to initialize bevy renderer: {e}"))
            }
            #[cfg(not(feature = "renderer-bevy"))]
            {
                Err("this binary was built without the bevy backend \
                     (build with --features renderer-bevy)"
                    .into())
            }
        }
        other => Err(format!(
            "unknown renderer {other:?}; expected \"wgpu\" or \"bevy\" \
             (built backends: wgpu={}, bevy={})",
            cfg!(feature = "renderer-wgpu"),
            cfg!(feature = "renderer-bevy")
        )),
    }
}
