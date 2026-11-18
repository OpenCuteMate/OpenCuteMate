//! The character aggregate: identity, configuration, behavioral phase,
//! physics body and animation, behind one API.
//!
//! This is the single owner of character state — nothing else in the
//! workspace keeps per-character booleans. The runtime translates platform
//! events into semantic [`InteractionEvent`]s (via
//! [`crate::InteractionSystem`]) and feeds them here; renderers read the
//! resulting [`crate::AnimatedPose`].
//!
//! Positions are kept in logical pixels internally (the space physics,
//! drag clamping and output bounds share) and exposed normalized for the
//! renderer contract.

use std::time::Instant;

use ocm_core::CharacterConfig;

use crate::animation::{AnimatedPose, Animator};
use crate::desktop::DesktopBounds;
use crate::interaction::InteractionEvent;
use crate::model::{CharacterModel, ElementId};
use crate::physics::{self, Body};
use crate::state::Phase;

/// Result of feeding one interaction event into the character.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct InteractionOutcome {
    /// Visual state changed; a render is due.
    pub dirty: bool,
    /// A bounded animation or physical simulation just started; the
    /// scheduler should drive frames until [`Character::update`] says
    /// otherwise.
    pub needs_frames: bool,
}

/// Result of one simulation update.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct UpdateOutcome {
    /// State changed during this step; a render is due.
    pub dirty: bool,
    /// More updates are required (animation/reaction/physics in flight).
    pub needs_frames: bool,
}

/// Fraction of the output height the character's physical box spans
/// per unit of `size_frac` (the body box is square).
const PHYSICAL_HALF_FRAC: f64 = 0.5;
/// Toss velocity clamps, in output widths/heights per second.
const TOSS_MAX_WIDTHS_PER_S: f64 = 2.0;
const TOSS_MAX_HEIGHTS_PER_S: f64 = 2.5;
/// Drag smoothing factor for the live lean velocity.
const LEAN_SMOOTHING: f64 = 0.35;

/// A desktop companion character.
pub struct Character {
    model: CharacterModel,
    animator: Animator,
    /// The element the sleeping pose flattens (eyes); unused slots when
    /// the model has none.
    eyes: ElementId,
    size_frac: f32,
    animate: bool,
    physics: bool,

    phase: Phase,
    /// Element under the pointer — the hover *tint* channel, independent
    /// of the behavioral phase (a falling or dragged character can still
    /// be under the pointer).
    hover: Option<ElementId>,
    body: Body,
    bounds: DesktopBounds,

    /// Seconds in the current phase (one-shot clocks).
    phase_time: f32,
    /// Continuous clock, advanced only while frames are being produced.
    total_time: f32,

    /// Offset (logical px) from the anchor to the grabbed element's
    /// hitbox-rect center while dragging.
    drag_element_offset: (f64, f64),
    /// Smoothed horizontal velocity used for drag/fall lean (px/s).
    lean_velocity_x: f64,
    drag_last: Option<(Instant, f64)>,
}

impl Character {
    /// Create a character from its configuration and model. With physics
    /// enabled and the configured position above the floor, it starts
    /// falling immediately (the scheduler picks that up from
    /// [`Character::update`]).
    pub fn new(config: &CharacterConfig, model: CharacterModel, bounds: DesktopBounds) -> Self {
        let size_frac = config.size.clamp(0.02, 1.0);
        let (w, h) = (bounds.width as f64, bounds.height as f64);
        let position = (
            (config.position_x.clamp(0.0, 1.0) as f64) * w,
            (config.position_y.clamp(0.0, 1.0) as f64) * h,
        );
        let eyes = model
            .elements()
            .iter()
            .position(|e| e.name == "eyes")
            .map(|i| ElementId(i as u32))
            .unwrap_or(ElementId(0));
        let ground = h - size_frac as f64 * h * PHYSICAL_HALF_FRAC;
        let (airborne, phase) = if config.physics && position.1 < ground - 1.0 {
            (true, Phase::Falling)
        } else {
            (false, Phase::Idle)
        };
        Self {
            model,
            animator: Animator,
            eyes,
            size_frac,
            animate: config.animate,
            physics: config.physics,
            phase,
            hover: None,
            body: Body {
                position,
                velocity: (0.0, 0.0),
                airborne,
            },
            bounds,
            phase_time: 0.0,
            total_time: 0.0,
            drag_element_offset: (0.0, 0.0),
            lean_velocity_x: 0.0,
            drag_last: None,
        }
    }

    // --- accessors ---------------------------------------------------------

    pub fn model(&self) -> &CharacterModel {
        &self.model
    }

    pub fn phase(&self) -> Phase {
        self.phase
    }

    /// Element under the pointer, for the hover tint.
    pub fn hovered_element(&self) -> Option<ElementId> {
        self.hover
    }

    pub fn is_dragging(&self) -> bool {
        matches!(self.phase, Phase::Dragging { .. })
    }

    /// Character anchor, normalized [0,1] x [0,1] (top-left origin).
    pub fn position(&self) -> (f64, f64) {
        (
            self.body.position.0 / self.bounds.width as f64,
            self.body.position.1 / self.bounds.height as f64,
        )
    }

    pub fn size_frac(&self) -> f32 {
        self.size_frac
    }

    /// The half-extent of the character's physical box, logical px.
    fn half_extent_px(&self) -> (f64, f64) {
        let half = self.size_frac as f64 * self.bounds.height as f64 * PHYSICAL_HALF_FRAC;
        (half, half)
    }

    // --- interaction -------------------------------------------------------

    /// Feed one semantic interaction event. `now` timestamps the event for
    /// live drag-velocity sampling.
    pub fn handle_interaction(
        &mut self,
        now: Instant,
        event: &InteractionEvent,
    ) -> InteractionOutcome {
        match *event {
            InteractionEvent::HoverChanged { element } => {
                self.hover = element;
                tracing::debug!(?element, "hover state changed");
                let dirty = true;
                match (self.phase, element) {
                    // Waking up on any pointer contact.
                    (Phase::Sleeping, Some(e)) => {
                        self.enter(Phase::Hover { element: e });
                        tracing::debug!("character waking up (pointer contact)");
                    }
                    // Hovering a different element re-pops the reaction.
                    (Phase::Hover { .. }, Some(e)) => self.enter(Phase::Hover { element: e }),
                    (Phase::Idle, Some(e)) => self.enter(Phase::Hover { element: e }),
                    (Phase::Hover { .. }, None) => self.enter(Phase::Idle),
                    _ => {}
                }
                let needs = self
                    .animator
                    .one_shot_remaining(&self.phase, self.phase_time);
                InteractionOutcome {
                    dirty,
                    needs_frames: needs.is_some(),
                }
            }
            InteractionEvent::Press { element, button } => {
                // No phase change yet; future characters may react to
                // presses (grab highlight, sound, ...). Right presses are
                // the runtime's exit affordance.
                tracing::debug!(?element, ?button, "character pressed");
                InteractionOutcome::default()
            }
            InteractionEvent::Click { element, button } => {
                if !matches!(button, ocm_platform::PointerButton::Left) {
                    return InteractionOutcome::default();
                }
                let role = self.model.element(element).role.clone();
                tracing::info!(
                    element = %self.model.element(element).name,
                    ?role,
                    phase = self.phase.name(),
                    "character clicked"
                );
                self.enter(Phase::Interacting { element });
                InteractionOutcome {
                    dirty: true,
                    needs_frames: true,
                }
            }
            InteractionEvent::DragStart { element, .. } => {
                // The interaction system states positions in the grabbed
                // element's hitbox-rect space; map back to the anchor via
                // the model's element offset (logical px). The renderer
                // places hitbox rect centers exactly at these offsets, so
                // the mapping is exact.
                let h_px = self.size_frac as f64 * self.bounds.height as f64;
                let (id, name, offset) = {
                    let element = self.model.element(element);
                    (element.id, element.name.clone(), element.offset)
                };
                self.drag_element_offset = (offset[0] as f64 * h_px, -offset[1] as f64 * h_px);
                self.body.velocity = (0.0, 0.0);
                self.body.airborne = false;
                self.lean_velocity_x = 0.0;
                self.drag_last = None;
                self.enter(Phase::Dragging { element: id });
                tracing::info!(element = %name, "drag started");
                InteractionOutcome {
                    dirty: true,
                    needs_frames: false,
                }
            }
            InteractionEvent::DragMove { center_px } => {
                if !self.is_dragging() {
                    return InteractionOutcome::default();
                }
                let anchor = (
                    center_px.0 - self.drag_element_offset.0,
                    center_px.1 - self.drag_element_offset.1,
                );
                self.set_anchor_px(anchor);
                // Live lean velocity from consecutive samples.
                if let Some((t0, x0)) = self.drag_last {
                    let dt = now.saturating_duration_since(t0).as_secs_f64();
                    if dt > 1e-3 {
                        let vx = (self.body.position.0 - x0) / dt;
                        self.lean_velocity_x =
                            self.lean_velocity_x * (1.0 - LEAN_SMOOTHING) + vx * LEAN_SMOOTHING;
                    }
                }
                self.drag_last = Some((now, self.body.position.0));
                InteractionOutcome {
                    dirty: true,
                    needs_frames: false,
                }
            }
            InteractionEvent::DragEnd { element, velocity } => {
                self.drag_last = None;
                let released_on_ground = self.body.position.1 >= self.ground_px() - 1.0;
                if self.physics {
                    let (w, h) = (self.bounds.width as f64, self.bounds.height as f64);
                    self.body.velocity = (
                        velocity
                            .0
                            .clamp(-TOSS_MAX_WIDTHS_PER_S * w, TOSS_MAX_WIDTHS_PER_S * w),
                        velocity
                            .1
                            .clamp(-TOSS_MAX_HEIGHTS_PER_S * h, TOSS_MAX_HEIGHTS_PER_S * h),
                    );
                    if released_on_ground {
                        // Keep only the horizontal slide on the floor.
                        self.body.velocity.1 = 0.0;
                        self.body.airborne = false;
                        self.enter(self.base_phase());
                    } else {
                        self.body.airborne = true;
                        self.enter(Phase::Falling);
                    }
                } else {
                    // Without physics the character rests exactly where it
                    // was dropped (the validated Phase 2 behavior).
                    self.body.velocity = (0.0, 0.0);
                    self.enter(self.base_phase());
                }
                tracing::info!(
                    element = %self.model.element(element).name,
                    position = ?self.position(),
                    velocity = ?self.body.velocity,
                    "drag ended"
                );
                let needs = self.physics && !self.body.settled();
                InteractionOutcome {
                    dirty: true,
                    needs_frames: needs,
                }
            }
        }
    }

    /// Phase to return to when a drag or reaction ends.
    fn base_phase(&self) -> Phase {
        match self.hover {
            Some(element) => Phase::Hover { element },
            None => Phase::Idle,
        }
    }

    fn enter(&mut self, phase: Phase) {
        if self.phase != phase {
            tracing::debug!(from = self.phase.name(), to = phase.name(), "phase change");
            self.phase = phase;
            self.phase_time = 0.0;
        }
    }

    /// Clamp the anchor so the physical box stays inside the output.
    fn set_anchor_px(&mut self, anchor: (f64, f64)) {
        let (half_w, half_h) = self.half_extent_px();
        let (w, h) = (self.bounds.width as f64, self.bounds.height as f64);
        self.body.position = (
            anchor.0.clamp(half_w, (w - half_w).max(half_w)),
            anchor.1.clamp(half_h, (h - half_h).max(half_h)),
        );
    }

    fn ground_px(&self) -> f64 {
        self.bounds.height as f64 - self.half_extent_px().1
    }

    // --- simulation ---------------------------------------------------------

    /// Advance the character by `dt` seconds. Only meaningful while the
    /// scheduler is producing frames; a parked character advances
    /// nothing (its clocks are frozen by design).
    pub fn update(&mut self, dt: f32) -> UpdateOutcome {
        self.phase_time += dt;
        self.total_time += dt;

        if self.is_dragging() {
            // Position is pointer-driven; a motionless drag needs no
            // frames.
            return UpdateOutcome {
                dirty: false,
                needs_frames: self.wants_frames(),
            };
        }

        let mut dirty = false;
        let before_position = self.body.position;

        // The click reaction ends by itself.
        if matches!(self.phase, Phase::Interacting { .. })
            && self
                .animator
                .one_shot_remaining(&self.phase, self.phase_time)
                .is_none()
        {
            self.enter(self.base_phase());
            dirty = true;
        }

        if self.physics {
            let half = self.half_extent_px();
            let events = self.body.step(dt, &self.bounds, half);
            for event in &events {
                match event {
                    physics::PhysicsEvent::Landed => {
                        tracing::info!(position = ?self.position(), "character landed");
                        if self.phase == Phase::Falling {
                            self.enter(self.base_phase());
                        }
                        dirty = true;
                    }
                    physics::PhysicsEvent::BouncedGround | physics::PhysicsEvent::HitWall => {
                        tracing::debug!(?event, "physics bounce");
                    }
                    physics::PhysicsEvent::Rest => {
                        dirty = true;
                    }
                }
            }
            dirty |= self.body.position != before_position;
        }

        let animating_pose = self.animate
            || self
                .animator
                .one_shot_remaining(&self.phase, self.phase_time)
                .is_some();
        dirty |= animating_pose;

        UpdateOutcome {
            dirty,
            needs_frames: self.wants_frames(),
        }
    }

    /// Whether the character currently requires frame updates.
    pub fn wants_frames(&self) -> bool {
        self.animate
            || self
                .animator
                .one_shot_remaining(&self.phase, self.phase_time)
                .is_some()
            || (self.physics && !self.body.settled())
    }

    /// Enter the long-idle sleeping state, if resting.
    pub fn try_sleep(&mut self) -> bool {
        if self.phase == Phase::Idle {
            self.enter(Phase::Sleeping);
            tracing::info!("character sleeping");
            true
        } else {
            false
        }
    }

    /// Apply new output bounds (e.g. a layer-shell configure): re-clamps
    /// the character into the output; with physics, a character now above
    /// the floor starts falling.
    pub fn set_bounds(&mut self, bounds: DesktopBounds) -> UpdateOutcome {
        self.bounds = bounds;
        let before = self.body.position;
        self.set_anchor_px(self.body.position);
        let moved = self.body.position != before;
        let mut needs_frames = false;
        if self.physics
            && !matches!(self.phase, Phase::Dragging { .. })
            && self.body.position.1 < self.ground_px() - 1.0
        {
            self.body.airborne = true;
            self.enter(Phase::Falling);
            needs_frames = true;
        }
        UpdateOutcome {
            dirty: moved || needs_frames,
            needs_frames,
        }
    }

    // --- rendering ----------------------------------------------------------

    /// The fully resolved pose for the current state.
    pub fn pose(&self) -> AnimatedPose {
        let ctx = crate::animation::AnimContext {
            phase_time: self.phase_time,
            total_time: self.total_time,
            animate: self.animate,
            lean_velocity_x: if self.is_dragging() {
                self.lean_velocity_x
            } else {
                self.body.velocity.0
            },
            output_height: self.bounds.height as f64,
        };
        let mut pose = self.animator.pose(&self.phase, &ctx, self.eyes);
        // Hover tint is a channel independent of the phase: brighten the
        // hovered element even while falling or dragging.
        if let Some(element) = self.hover {
            if let Some(slot) = pose.elements.get_mut(element.0 as usize) {
                slot.tint = [1.22, 1.16, 1.06];
            }
        }
        pose
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::interaction::{ElementHitbox, InteractionSystem};
    use crate::model::prototype_cube;
    use ocm_platform::{PointerButton, PointerEvent, PointerEventKind, Rect};

    const W: u32 = 1600;
    const H: u32 = 900;
    const BOUNDS: DesktopBounds = DesktopBounds {
        width: W,
        height: H,
    };

    fn config(physics: bool) -> CharacterConfig {
        CharacterConfig {
            position_x: 0.5,
            position_y: 0.5,
            size: 0.4,
            animate: false,
            physics,
            ..Default::default()
        }
    }

    fn character(physics: bool) -> Character {
        Character::new(&config(physics), prototype_cube(), BOUNDS)
    }

    /// Anchor px for a normalized position.
    fn px(u: f64, v: f64) -> (f64, f64) {
        (u * W as f64, v * H as f64)
    }

    /// Prototype-cube hitboxes at the character's current position, as the
    /// renderer would produce them (rect centers at the element offsets).
    fn hitboxes(character: &Character) -> Vec<ElementHitbox> {
        let h_px = character.size_frac() as f64 * H as f64;
        let (ax, ay) = px(character.position().0, character.position().1);
        let margin = std::f64::consts::SQRT_2 * 1.12;
        character
            .model()
            .hit_priority()
            .map(|element| {
                let extent = element.hit_extent.expect("interactive");
                let cx = ax + element.offset[0] as f64 * h_px;
                let cy = ay - element.offset[1] as f64 * h_px;
                let rx = extent[0] as f64 * h_px * margin;
                let ry = extent[1] as f64 * h_px * margin;
                ElementHitbox {
                    id: element.id,
                    rect: Rect {
                        x: (cx - rx).floor() as i32,
                        y: (cy - ry).floor() as i32,
                        width: (rx * 2.0).ceil() as i32,
                        height: (ry * 2.0).ceil() as i32,
                    },
                }
            })
            .collect()
    }

    fn at(ms: u64) -> Instant {
        Instant::now() + std::time::Duration::from_millis(ms)
    }

    /// Drive a full scripted pointer sequence through the interaction
    /// system and the character, mirroring the runtime's wiring.
    struct Harness {
        interaction: InteractionSystem,
        character: Character,
    }

    impl Harness {
        fn new(physics: bool) -> Self {
            Self {
                interaction: InteractionSystem::new(),
                character: character(physics),
            }
        }

        fn pointer(&mut self, now: Instant, kind: PointerEventKind, x: f64, y: f64) {
            let event = PointerEvent { kind, x, y };
            let hitboxes = hitboxes(&self.character);
            for interaction in self.interaction.on_pointer(now, &event, &hitboxes) {
                let _ = self.character.handle_interaction(now, &interaction);
            }
        }
    }

    #[test]
    fn starts_idle_and_settled() {
        let mut character = character(false);
        assert_eq!(character.phase(), Phase::Idle);
        assert_eq!(character.hovered_element(), None);
        assert!(!character.wants_frames());
        let outcome = character.update(1.0 / 60.0);
        assert_eq!(
            outcome,
            UpdateOutcome {
                dirty: false,
                needs_frames: false
            }
        );
    }

    #[test]
    fn hover_enter_and_leave_transition_the_phase() {
        let mut harness = Harness::new(false);
        let (x, y) = px(0.5, 0.5);

        harness.pointer(at(0), PointerEventKind::Enter, x, y);
        assert_eq!(
            harness.character.phase(),
            Phase::Hover {
                element: ElementId(4) // face: the anchor sits on the face rect
            }
        );
        assert!(harness.character.wants_frames(), "hover pop runs");

        // Pop completes after its window.
        for _ in 0..30 {
            let _ = harness.character.update(1.0 / 60.0);
        }
        assert!(!harness.character.wants_frames());
        assert_eq!(harness.character.hovered_element(), Some(ElementId(4)));

        harness.pointer(at(400), PointerEventKind::Leave, x, y);
        assert_eq!(harness.character.phase(), Phase::Idle);
        assert_eq!(harness.character.hovered_element(), None);
    }

    #[test]
    fn hover_on_the_eyes_reports_the_eyes_element() {
        let mut harness = Harness::new(false);
        // Eyes element sits at (0, +0.14)·h above the anchor.
        harness.pointer(at(0), PointerEventKind::Enter, 800.0, 450.0 - 0.14 * 360.0);
        assert_eq!(
            harness.character.hovered_element(),
            Some(ElementId(5)),
            "the eyes hit-priority slot must win over the face"
        );
    }

    #[test]
    fn click_triggers_a_bounded_interaction_reaction() {
        let mut harness = Harness::new(false);
        let (x, y) = px(0.5, 0.5);
        harness.pointer(at(0), PointerEventKind::Enter, x, y);
        harness.pointer(
            at(10),
            PointerEventKind::Press {
                button: PointerButton::Left,
            },
            x,
            y,
        );
        harness.pointer(
            at(30),
            PointerEventKind::Release {
                button: PointerButton::Left,
            },
            x,
            y,
        );

        assert_eq!(
            harness.character.phase(),
            Phase::Interacting {
                element: ElementId(4)
            }
        );
        assert!(harness.character.wants_frames());

        // The reaction ends and returns to the hovered base phase.
        for _ in 0..40 {
            let _ = harness.character.update(1.0 / 60.0);
        }
        assert_eq!(
            harness.character.phase(),
            Phase::Hover {
                element: ElementId(4)
            }
        );
        assert!(!harness.character.wants_frames());
    }

    #[test]
    fn drag_moves_the_character_with_the_pointer() {
        let mut harness = Harness::new(false);
        let (x, y) = px(0.5, 0.5);
        harness.pointer(at(0), PointerEventKind::Enter, x, y);
        harness.pointer(
            at(10),
            PointerEventKind::Press {
                button: PointerButton::Left,
            },
            x,
            y,
        );

        // Motion beyond the threshold promotes to a drag.
        harness.pointer(at(30), PointerEventKind::Motion, x + 40.0, y + 30.0);
        assert!(harness.character.is_dragging());

        // A long drag far off the original hitbox still tracks: the
        // character follows, clamped so the body stays on the output.
        for i in 1..=30 {
            let t = at(30 + i * 10);
            harness.pointer(
                t,
                PointerEventKind::Motion,
                x + 40.0 + 50.0 * i as f64,
                y + 30.0 + 10.0 * i as f64,
            );
        }
        let last = harness.character.position();
        // The anchor is clamped so the body stays fully on the output.
        let clamp_u = 1.0 - (0.4 * H as f64 / 2.0) / W as f64;
        assert!(
            (last.0 - clamp_u).abs() < 1e-6,
            "drag must track the pointer to the wall clamp (got {last:?})"
        );

        harness.pointer(
            at(500),
            PointerEventKind::Release {
                button: PointerButton::Left,
            },
            3000.0,
            900.0,
        );
        assert!(!harness.character.is_dragging());
        // Without physics the character rests exactly where it was
        // dropped (the validated Phase 2 behavior).
        assert_eq!(harness.character.position(), last);
    }

    #[test]
    fn drag_positions_follow_the_grab_anchor_exactly() {
        let mut harness = Harness::new(false);
        let (x, y) = px(0.5, 0.5);
        harness.pointer(at(0), PointerEventKind::Enter, x, y);
        // Grab the face element off-center by (10, 5).
        harness.pointer(
            at(10),
            PointerEventKind::Press {
                button: PointerButton::Left,
            },
            x + 10.0,
            y + 5.0,
        );
        harness.pointer(at(30), PointerEventKind::Motion, x + 50.0, y + 25.0);

        // The anchor keeps exactly the offset it had at grab time: the
        // pointer moved (40, 20) since the press, so the anchor did too.
        let anchor = harness.character.position();
        let anchor_px = px(anchor.0, anchor.1);
        assert!(
            (anchor_px.0 - (x + 40.0)).abs() < 1.0 && (anchor_px.1 - (y + 20.0)).abs() < 1.0,
            "anchor {anchor_px:?}, expected grab-offset-preserving ({}, {})",
            x + 40.0,
            y + 20.0
        );
    }

    #[test]
    fn right_press_does_not_change_the_phase() {
        let mut harness = Harness::new(false);
        let (x, y) = px(0.5, 0.5);
        harness.pointer(at(0), PointerEventKind::Enter, x, y);
        harness.pointer(
            at(10),
            PointerEventKind::Press {
                button: PointerButton::Right,
            },
            x,
            y,
        );
        assert_eq!(
            harness.character.phase(),
            Phase::Hover {
                element: ElementId(4)
            }
        );
    }

    #[test]
    fn physics_release_above_ground_falls_and_lands() {
        let mut harness = Harness::new(true);
        // The spawn point (0.5, 0.5) is above the floor for size 0.4.
        assert_eq!(harness.character.phase(), Phase::Falling);
        assert!(harness.character.wants_frames());

        // Step the fall at 60 fps until it lands.
        let mut landed = false;
        for _ in 0..300 {
            let outcome = harness.character.update(1.0 / 60.0);
            if outcome.dirty && !harness.character.wants_frames() {
                landed = true;
                break;
            }
            assert!(harness.character.wants_frames(), "never landed");
        }
        assert!(landed);
        assert_eq!(harness.character.phase(), Phase::Idle);
        let ground = 1.0 - 0.4 / 2.0;
        assert!(
            (harness.character.position().1 - ground).abs() < 0.01,
            "anchor must rest on the floor (got {:?})",
            harness.character.position()
        );
    }

    #[test]
    fn physics_toss_after_drag_release_travels_and_settles() {
        let mut harness = Harness::new(true);
        // Fast-forward to the ground.
        for _ in 0..300 {
            if !harness.character.wants_frames() {
                break;
            }
            let _ = harness.character.update(1.0 / 60.0);
        }
        let ground_x = harness.character.position().0;
        let (x, y) = px(ground_x, 1.0 - 0.2);

        // Drag fast sideways and release mid-motion.
        harness.pointer(at(0), PointerEventKind::Enter, x, y);
        harness.pointer(
            at(10),
            PointerEventKind::Press {
                button: PointerButton::Left,
            },
            x,
            y,
        );
        for i in 1..=6 {
            harness.pointer(
                at(20 + i * 15),
                PointerEventKind::Motion,
                x - 80.0 * i as f64,
                y,
            );
        }
        let release_x = x - 80.0 * 6.0;
        harness.pointer(
            at(150),
            PointerEventKind::Release {
                button: PointerButton::Left,
            },
            release_x,
            y,
        );

        // Released while grounded: an immediate slide, no fall. The
        // character is still under the pointer (the drop position), so
        // the base phase is a hover on the grabbed element's region.
        assert_eq!(
            harness.character.phase(),
            Phase::Hover {
                element: ElementId(4)
            }
        );
        assert!(harness.character.wants_frames(), "slide must be simulated");

        for _ in 0..600 {
            if !harness.character.wants_frames() {
                break;
            }
            let _ = harness.character.update(1.0 / 60.0);
        }
        assert!(!harness.character.wants_frames(), "never settled");
        let final_x = harness.character.position().0;
        assert!(
            final_x < ground_x - 0.01,
            "the toss velocity must carry the character left (start {ground_x}, end {final_x})"
        );
    }

    #[test]
    fn sleeping_wakes_on_pointer_contact() {
        let mut harness = Harness::new(false);
        assert!(harness.character.try_sleep());
        assert_eq!(harness.character.phase(), Phase::Sleeping);
        let pose = harness.character.pose();
        assert_eq!(pose.element(ElementId(5)).scale, [1.0, 0.15, 1.0]);

        let (x, y) = px(0.5, 0.5);
        harness.pointer(at(0), PointerEventKind::Enter, x, y);
        assert_eq!(
            harness.character.phase(),
            Phase::Hover {
                element: ElementId(4)
            }
        );
        // Eyes reopened.
        assert_eq!(
            harness.character.pose().element(ElementId(5)).scale,
            [1.0, 1.0, 1.0]
        );
    }

    #[test]
    fn sleep_is_refused_unless_idle() {
        let mut harness = Harness::new(false);
        let (x, y) = px(0.5, 0.5);
        harness.pointer(at(0), PointerEventKind::Enter, x, y);
        assert!(!harness.character.try_sleep(), "hovering never sleeps");
        assert_eq!(
            harness.character.phase(),
            Phase::Hover {
                element: ElementId(4)
            }
        );
    }

    #[test]
    fn bounds_growth_makes_a_physics_character_fall() {
        let mut character = character(true);
        // Move to the ground first.
        for _ in 0..300 {
            if !character.wants_frames() {
                break;
            }
            let _ = character.update(1.0 / 60.0);
        }
        assert_eq!(character.phase(), Phase::Idle);

        let bigger = DesktopBounds {
            width: W,
            height: H * 2,
        };
        let outcome = character.set_bounds(bigger);
        assert!(outcome.needs_frames);
        assert_eq!(character.phase(), Phase::Falling);
    }

    #[test]
    fn bounds_shrink_clamps_the_character_inside() {
        let mut character = character(false);
        let smaller = DesktopBounds {
            width: W / 2,
            height: H / 2,
        };
        let outcome = character.set_bounds(smaller);
        assert!(outcome.dirty);
        let (u, _v) = character.position();
        let half_u = (0.4 * (H / 2) as f64 / 2.0) / (W / 2) as f64;
        assert!(
            (half_u..=1.0 - half_u).contains(&u),
            "anchor {u} must keep the body inside the shrunken output"
        );
    }

    #[test]
    fn animate_mode_needs_frames_and_bobs() {
        let config = CharacterConfig {
            animate: true,
            ..config(false)
        };
        let mut character = Character::new(&config, prototype_cube(), BOUNDS);
        assert!(character.wants_frames());
        character.update(1.0 / 60.0);
        assert!(character.update(1.0 / 60.0).dirty);
        let pose = character.pose();
        assert!(pose.root.offset_up != 0.0 || pose.root.yaw != 0.0);
    }

    #[test]
    fn hover_tint_tracks_the_hovered_element_independently_of_phase() {
        let mut harness = Harness::new(false);
        let (x, y) = px(0.5, 0.5);
        harness.pointer(at(0), PointerEventKind::Enter, x, y);
        // During the hover pop the phase is Hover and the tint is set.
        let pose = harness.character.pose();
        assert_eq!(pose.element(ElementId(4)).tint, [1.22, 1.16, 1.06]);
        assert_eq!(pose.element(ElementId(1)).tint, [1.0, 1.0, 1.0]);
    }
}
