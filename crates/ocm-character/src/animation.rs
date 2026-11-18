//! Animation abstraction.
//!
//! The seam between behavior and rendering: the character system decides
//! *what* is happening (a [`Phase`] and its context) and this module turns
//! it into a concrete [`AnimatedPose`] — a fully resolved, renderer-neutral
//! snapshot. Renderers consume poses; they never interpret character
//! state, and this module never touches a GPU type.
//!
//! The prototype supplies procedural animations ([`Animator`]). The same
//! contract is the integration point for future model-specific animation:
//! a glTF clip player or VRM expression controller maps [`Phase`]s onto
//! clips/expressions and produces the same [`AnimatedPose`] (growing the
//! per-element slots when skeletons arrive — see
//! `docs/character-architecture.md`).
//!
//! Scheduling contract: every animation here is either **continuous**
//! (the legacy `animate` bob+spin, physics) or a **bounded one-shot**
//! (hover pop, click squash). [`Animator::active`] reports exactly how
//! long the current one-shot still needs frames; the runtime parks
//! otherwise. An idle character with animation disabled animates nothing.

use crate::model::ElementId;
use crate::state::Phase;

/// Root transform adjustments, in character-height units (offsets) and
/// radians (rotation). Applied by renderers on top of the anchor
/// placement: offset along the camera right/up axes, then yaw (world Y),
/// pitch (camera-right axis), roll (view axis), then scale.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RootPose {
    /// Offset along the camera right axis, character-height units.
    pub offset_right: f32,
    /// Offset along the camera up axis, character-height units.
    pub offset_up: f32,
    pub yaw: f32,
    pub pitch: f32,
    pub roll: f32,
    /// Per-axis scale (squash & stretch); `[1.0; 3]` is neutral.
    pub scale: [f32; 3],
}

impl Default for RootPose {
    fn default() -> Self {
        Self::NEUTRAL
    }
}

impl RootPose {
    pub const NEUTRAL: RootPose = RootPose {
        offset_right: 0.0,
        offset_up: 0.0,
        yaw: 0.0,
        pitch: 0.0,
        roll: 0.0,
        scale: [1.0, 1.0, 1.0],
    };
}

/// Per-element animation slot: element-local scale and tint multiplier.
/// Indexed by [`ElementId`] in [`AnimatedPose::elements`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ElementPose {
    /// Multiplier on the element's local scale.
    pub scale: [f32; 3],
    /// Color multiplier (1.0 = base color).
    pub tint: [f32; 3],
}

impl ElementPose {
    pub const NEUTRAL: ElementPose = ElementPose {
        scale: [1.0, 1.0, 1.0],
        tint: [1.0, 1.0, 1.0],
    };
}

/// One frame's fully resolved animation state.
///
/// `elements` is parallel to the model's element table (`element(id)`
/// indexes this vector by `id.0`).
#[derive(Debug, Clone, PartialEq)]
pub struct AnimatedPose {
    pub root: RootPose,
    pub elements: Vec<ElementPose>,
}

impl AnimatedPose {
    /// A pose that changes nothing, sized for `element_count` slots.
    pub fn neutral(element_count: usize) -> Self {
        Self {
            root: RootPose::NEUTRAL,
            elements: vec![ElementPose::NEUTRAL; element_count],
        }
    }

    pub fn element(&self, id: ElementId) -> &ElementPose {
        self.elements
            .get(id.0 as usize)
            .unwrap_or(&ElementPose::NEUTRAL)
    }
}

/// Duration of the one-shot hover "notice me" pop.
pub const HOVER_POP_SECS: f32 = 0.30;
/// Duration of the one-shot click squash reaction.
pub const CLICK_REACTION_SECS: f32 = 0.35;
/// Extra root scale while grabbed.
const DRAG_GRAB_SCALE: f32 = 1.05;
/// Maximum drag tilt, radians.
const DRAG_MAX_TILT: f32 = 0.30;
/// Falling nose-down pitch, radians.
const FALL_PITCH: f32 = 0.22;
/// Sleeping slump pitch, radians.
const SLEEP_PITCH: f32 = 0.10;

/// Context the animator needs beyond the phase.
#[derive(Debug, Clone, Copy)]
pub struct AnimContext {
    /// Seconds spent in the current phase (one-shot clocks).
    pub phase_time: f32,
    /// Total clock (drives the continuous bob + spin), only advanced
    /// while the character requested frames.
    pub total_time: f32,
    /// Continuous idle animation enabled (legacy `--animate`).
    pub animate: bool,
    /// Horizontal velocity in logical px/s the character should lean with
    /// (live drag velocity while dragging, body velocity while falling).
    pub lean_velocity_x: f64,
    /// Output height in logical px (normalizes velocities).
    pub output_height: f64,
}

/// Procedural animator for box-part characters (the prototype cube).
///
/// Future characters supply their own mapping from the same
/// [`Phase`]/[`AnimContext`] inputs to [`AnimatedPose`] output — e.g. an
/// imported model's clip table or VRM expressions.
pub struct Animator;

impl Animator {
    /// Resolve the current pose.
    pub fn pose(&self, phase: &Phase, ctx: &AnimContext, eyes: ElementId) -> AnimatedPose {
        let mut pose = AnimatedPose::neutral(eyes.0 as usize + 1);
        match phase {
            Phase::Idle | Phase::Hover { .. } => {
                if ctx.animate {
                    // Legacy Phase 2 bob + spin, preserved verbatim.
                    pose.root.offset_up = 0.055 * (ctx.total_time * 2.2).sin();
                    pose.root.yaw = ctx.total_time * 0.8;
                }
                if let Phase::Hover { .. } = phase {
                    // One-shot pop when the hover began.
                    let t = ctx.phase_time.min(HOVER_POP_SECS);
                    let k = (std::f32::consts::PI * t / HOVER_POP_SECS).sin();
                    pose.root.scale[0] += 0.05 * k;
                    pose.root.scale[1] += 0.05 * k;
                }
            }
            Phase::Dragging { .. } => {
                pose.root.scale = [DRAG_GRAB_SCALE, DRAG_GRAB_SCALE, DRAG_GRAB_SCALE];
                // Lean into the direction of travel (screen-space roll:
                // dragging right tips the top toward +x = negative roll).
                let v_norm = (ctx.lean_velocity_x / ctx.output_height.max(1.0)).clamp(-2.0, 2.0);
                pose.root.roll = -DRAG_MAX_TILT * (v_norm / 2.0) as f32;
            }
            Phase::Interacting { .. } => {
                // Squash & stretch: compress on impact, overshoot, settle.
                let t = ctx.phase_time.min(CLICK_REACTION_SECS);
                let k = (std::f32::consts::PI * t / CLICK_REACTION_SECS).sin();
                pose.root.scale = [1.0 + 0.14 * k, 1.0 - 0.20 * k, 1.0 + 0.14 * k];
            }
            Phase::Falling => {
                pose.root.pitch = FALL_PITCH;
                let v_norm = (ctx.lean_velocity_x / ctx.output_height.max(1.0)).clamp(-2.0, 2.0);
                pose.root.roll = -0.18 * (v_norm / 2.0) as f32;
            }
            Phase::Sleeping => {
                pose.root.pitch = SLEEP_PITCH;
                if let Some(slot) = pose.elements.get_mut(eyes.0 as usize) {
                    // Eyes closed (flattened) and dimmed.
                    slot.scale = [1.0, 0.15, 1.0];
                    slot.tint = [0.7, 0.7, 0.72];
                }
            }
        }
        pose
    }

    /// How much longer the current phase still needs frame updates.
    ///
    /// `None` = no one-shot animation running. Continuous needs are the
    /// caller's (physics, the `animate` flag).
    pub fn one_shot_remaining(&self, phase: &Phase, phase_time: f32) -> Option<f32> {
        match phase {
            Phase::Hover { .. } if phase_time < HOVER_POP_SECS => Some(HOVER_POP_SECS - phase_time),
            Phase::Interacting { .. } if phase_time < CLICK_REACTION_SECS => {
                Some(CLICK_REACTION_SECS - phase_time)
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx(animate: bool, phase_time: f32, total_time: f32) -> AnimContext {
        AnimContext {
            phase_time,
            total_time,
            animate,
            lean_velocity_x: 0.0,
            output_height: 900.0,
        }
    }

    #[test]
    fn idle_without_animation_is_exactly_neutral() {
        // The zero-cost idle invariant: no bob, no rotation, no scale.
        let pose = Animator.pose(&Phase::Idle, &ctx(false, 10.0, 10.0), ElementId(5));
        assert_eq!(pose.root, RootPose::NEUTRAL);
        assert!(pose.elements.iter().all(|e| *e == ElementPose::NEUTRAL));
        assert_eq!(Animator.one_shot_remaining(&Phase::Idle, 10.0), None);
    }

    #[test]
    fn animate_mode_reproduces_the_legacy_bob_and_spin() {
        // The Phase 2 formula, verbatim: bob = 0.055·sin(t·2.2),
        // yaw = 0.8·t.
        let t = 1.25_f32;
        let pose = Animator.pose(&Phase::Idle, &ctx(true, 0.0, t), ElementId(5));
        assert_eq!(pose.root.offset_up, 0.055 * (t * 2.2).sin());
        assert_eq!(pose.root.yaw, t * 0.8);
    }

    #[test]
    fn hover_runs_a_bounded_pop() {
        let phase = Phase::Hover {
            element: ElementId(4),
        };
        assert_eq!(
            Animator.one_shot_remaining(&phase, 0.0),
            Some(HOVER_POP_SECS)
        );
        assert_eq!(Animator.one_shot_remaining(&phase, 5.0), None);

        let pose = Animator.pose(&phase, &ctx(false, HOVER_POP_SECS / 2.0, 0.0), ElementId(5));
        assert!(
            pose.root.scale[1] > 1.0,
            "pops up mid-reaction (scale {:?})",
            pose.root.scale
        );
        let pose = Animator.pose(&phase, &ctx(false, HOVER_POP_SECS, 0.0), ElementId(5));
        assert_eq!(pose.root.scale, [1.0, 1.0, 1.0], "settles back to neutral");
    }

    #[test]
    fn click_reaction_is_a_bounded_squash_that_settles() {
        let phase = Phase::Interacting {
            element: ElementId(4),
        };
        let mid = Animator.pose(
            &phase,
            &ctx(false, CLICK_REACTION_SECS / 2.0, 0.0),
            ElementId(5),
        );
        assert!(mid.root.scale[1] < 1.0, "squashes mid-reaction");
        assert!(mid.root.scale[0] > 1.0);
        assert_eq!(
            Animator.one_shot_remaining(&phase, 0.0),
            Some(CLICK_REACTION_SECS)
        );
        assert_eq!(Animator.one_shot_remaining(&phase, 1.0), None);
        let end = Animator.pose(&phase, &ctx(false, CLICK_REACTION_SECS, 0.0), ElementId(5));
        assert_eq!(end.root.scale, [1.0, 1.0, 1.0]);
    }

    #[test]
    fn drag_tilts_with_velocity_and_grows() {
        let phase = Phase::Dragging {
            element: ElementId(4),
        };
        let mut context = ctx(false, 0.0, 0.0);
        context.lean_velocity_x = 900.0;
        let pose = Animator.pose(&phase, &context, ElementId(5));
        assert!(pose.root.roll < 0.0, "tilts right when dragged right");
        assert!(pose.root.roll > -DRAG_MAX_TILT, "half-saturated tilt");
        assert_eq!(pose.root.scale, [DRAG_GRAB_SCALE; 3]);
        // Clamped beyond saturation.
        context.lean_velocity_x = 100_000.0;
        let pose = Animator.pose(&phase, &context, ElementId(5));
        assert_eq!(pose.root.roll, -DRAG_MAX_TILT);
        assert_eq!(Animator.one_shot_remaining(&phase, 5.0), None);
    }

    #[test]
    fn falling_has_a_fixed_tumble() {
        let pose = Animator.pose(&Phase::Falling, &ctx(false, 0.0, 0.0), ElementId(5));
        assert_eq!(pose.root.pitch, FALL_PITCH);
        assert_eq!(Animator.one_shot_remaining(&Phase::Falling, 0.0), None);
    }

    #[test]
    fn sleeping_closes_the_eyes_element() {
        let eyes = ElementId(5);
        let pose = Animator.pose(&Phase::Sleeping, &ctx(false, 0.0, 0.0), eyes);
        assert_eq!(pose.root.pitch, SLEEP_PITCH);
        assert_eq!(pose.element(eyes).scale, [1.0, 0.15, 1.0]);
        assert_eq!(pose.element(ElementId(4)).scale, [1.0, 1.0, 1.0]);
    }

    #[test]
    fn neutral_pose_scales_with_element_count() {
        let pose = AnimatedPose::neutral(6);
        assert_eq!(pose.elements.len(), 6);
        assert_eq!(pose.elements[5], ElementPose::NEUTRAL);
        // Unknown ids resolve to neutral instead of panicking.
        assert_eq!(pose.element(ElementId(99)), &ElementPose::NEUTRAL);
    }
}
