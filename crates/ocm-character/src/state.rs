//! Character state machine.
//!
//! One explicit [`Phase`] at a time; transitions happen only through the
//! [`crate::Character`] aggregate, which is also the only place that
//! touches phase-adjacent data (position, velocity, clocks). No scattered
//! booleans.
//!
//! Not every character must implement every phase: the machine only ever
//! *enters* a phase when its trigger occurred (e.g. a character that is
//! never dragged never sees `Dragging`), and animators decide per phase
//! what to show — unknown phases fall back to the neutral pose.

use crate::model::ElementId;

/// Behavioral state of a character.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Phase {
    /// Default resting state.
    Idle,
    /// The pointer rests on an interactive element and no drag is active.
    Hover { element: ElementId },
    /// The character is being dragged; the pointer owns its position.
    Dragging { element: ElementId },
    /// Responding to a click on `element` (a bounded reaction).
    Interacting { element: ElementId },
    /// Falling under gravity (physics active).
    Falling,
    /// Long-idle resting state.
    Sleeping,
}

impl Phase {
    pub fn name(&self) -> &'static str {
        match self {
            Phase::Idle => "idle",
            Phase::Hover { .. } => "hover",
            Phase::Dragging { .. } => "dragging",
            Phase::Interacting { .. } => "interacting",
            Phase::Falling => "falling",
            Phase::Sleeping => "sleeping",
        }
    }
}
