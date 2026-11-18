//! Pointer interaction system: turns raw platform pointer events plus the
//! current per-element hitboxes into semantic [`InteractionEvent`]s.
//!
//! The system owns the mechanical interaction state (hover element, press,
//! active drag, pointer-velocity samples). It knows nothing about
//! characters, phases or rendering; consumers decide what events *mean*.
//!
//! Interaction vocabulary is entirely [`ElementId`]-based — the same events
//! fire for the prototype cube and a future imported VRM model.

use std::collections::VecDeque;
use std::time::Instant;

use ocm_platform::{PointerButton, PointerEvent, PointerEventKind, Rect};

use crate::model::ElementId;

/// One interactive element's hitbox in logical surface pixels.
///
/// Produced by the renderer from the same placement math that draws the
/// character (single source of truth); consumed here for hit-testing and
/// by the runtime for the compositor input region.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ElementHitbox {
    pub id: ElementId,
    /// Hit rect in logical surface pixels (top-left origin).
    pub rect: Rect,
}

impl ElementHitbox {
    pub fn contains(&self, x: f64, y: f64) -> bool {
        x >= self.rect.x as f64
            && y >= self.rect.y as f64
            && x < (self.rect.x + self.rect.width) as f64
            && y < (self.rect.y + self.rect.height) as f64
    }

    fn center(&self) -> (f64, f64) {
        (
            self.rect.x as f64 + self.rect.width as f64 / 2.0,
            self.rect.y as f64 + self.rect.height as f64 / 2.0,
        )
    }
}

/// Semantic interaction events, consumed by the character system.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum InteractionEvent {
    /// The element under the pointer changed (`None` = pointer left the
    /// character). Suppressed while dragging.
    HoverChanged { element: Option<ElementId> },
    /// A button went down on an element (a potential click or drag).
    Press {
        element: ElementId,
        button: PointerButton,
    },
    /// Press + release on an element without exceeding the drag motion
    /// threshold.
    Click {
        element: ElementId,
        button: PointerButton,
    },
    /// The press turned into a drag. `grab_offset` is the pointer position
    /// at press time relative to the grabbed element's hitbox-rect center
    /// (logical px); [`InteractionEvent::DragMove`] positions are stated
    /// in the same element-center space.
    DragStart {
        element: ElementId,
        grab_offset: (f64, f64),
    },
    /// Pointer motion while dragging: the desired position of the grabbed
    /// element's hitbox-rect center, in logical px. The character maps
    /// this to its own anchor via the model's element offsets.
    DragMove { center_px: (f64, f64) },
    /// The drag ended. `velocity` is the sampled pointer velocity in
    /// logical px/s (for a toss).
    DragEnd {
        element: ElementId,
        velocity: (f64, f64),
    },
}

/// Pointer motion beyond this distance (logical px) promotes a press into
/// a drag; below it, release counts as a click.
const DRAG_THRESHOLD_PX: f64 = 4.0;
/// Release velocity is sampled over this look-back window.
const VELOCITY_WINDOW: std::time::Duration = std::time::Duration::from_millis(120);

#[derive(Debug, Clone, Copy)]
struct PressState {
    element: ElementId,
    button: PointerButton,
    pos: (f64, f64),
    /// Pointer offset from the element hitbox-rect center at press time.
    grab_offset: (f64, f64),
}

#[derive(Debug, Clone, Copy)]
struct DragState {
    element: ElementId,
    grab_offset: (f64, f64),
}

/// Mechanical pointer-interaction state.
pub struct InteractionSystem {
    hover: Option<ElementId>,
    pressed: Option<PressState>,
    dragging: Option<DragState>,
    /// Recent pointer samples for release velocity.
    samples: VecDeque<(Instant, f64, f64)>,
    last_pos: (f64, f64),
}

impl Default for InteractionSystem {
    fn default() -> Self {
        Self::new()
    }
}

impl InteractionSystem {
    pub fn new() -> Self {
        Self {
            hover: None,
            pressed: None,
            dragging: None,
            samples: VecDeque::new(),
            last_pos: (0.0, 0.0),
        }
    }

    /// The element currently under the pointer, if any.
    pub fn hovered(&self) -> Option<ElementId> {
        self.hover
    }

    pub fn is_dragging(&self) -> bool {
        self.dragging.is_some()
    }

    /// Process one pointer event against the current hitboxes. `hitboxes`
    /// must be in hit-priority order (see [`crate::model::CharacterModel`]).
    pub fn on_pointer(
        &mut self,
        now: Instant,
        event: &PointerEvent,
        hitboxes: &[ElementHitbox],
    ) -> Vec<InteractionEvent> {
        let mut events = Vec::new();
        match event.kind {
            PointerEventKind::Enter | PointerEventKind::Motion => {
                self.last_pos = (event.x, event.y);
                self.push_sample(now, event.x, event.y);
                self.on_position(event.x, event.y, hitboxes, &mut events);
            }
            PointerEventKind::Leave => {
                // With the runtime's full-surface input region a leave
                // cannot occur mid-drag; if one ever arrives anyway, keep
                // the drag alive (pointer focus may return).
                if self.dragging.is_none() && self.hover.take().is_some() {
                    events.push(InteractionEvent::HoverChanged { element: None });
                }
                // A press that wandered off the hitbox without reaching
                // the drag threshold is aborted (the compositor delivered
                // no further motion to it).
                if self.dragging.is_none() {
                    self.pressed = None;
                }
            }
            PointerEventKind::Press { button } => {
                // Button events carry the current pointer position; keep
                // last_pos authoritative for the release-time recompute.
                self.last_pos = (event.x, event.y);
                if let Some(hit) = hitboxes.iter().find(|h| h.contains(event.x, event.y)) {
                    let center = hit.center();
                    let press = PressState {
                        element: hit.id,
                        button,
                        pos: (event.x, event.y),
                        grab_offset: (event.x - center.0, event.y - center.1),
                    };
                    self.pressed = Some(press);
                    self.samples.clear();
                    self.push_sample(now, event.x, event.y);
                    events.push(InteractionEvent::Press {
                        element: hit.id,
                        button,
                    });
                }
            }
            PointerEventKind::Release { button } => {
                self.last_pos = (event.x, event.y);
                if let Some(drag) = self.dragging.take() {
                    self.pressed = None;
                    let velocity = self.release_velocity(now);
                    events.push(InteractionEvent::DragEnd {
                        element: drag.element,
                        velocity,
                    });
                    // The pointer is wherever the drag left it; recompute
                    // hover against the (post-drag) hitboxes.
                    let hit = hit_test(hitboxes, self.last_pos.0, self.last_pos.1);
                    if hit != self.hover {
                        self.hover = hit;
                        events.push(InteractionEvent::HoverChanged { element: hit });
                    }
                } else if let Some(press) = self.pressed.take() {
                    if press.button == button {
                        events.push(InteractionEvent::Click {
                            element: press.element,
                            button,
                        });
                    }
                }
            }
        }
        events
    }

    /// Pointer moved to (x, y): drag follow, press promotion or hover
    /// tracking.
    fn on_position(
        &mut self,
        x: f64,
        y: f64,
        hitboxes: &[ElementHitbox],
        events: &mut Vec<InteractionEvent>,
    ) {
        if let Some(drag) = self.dragging {
            events.push(InteractionEvent::DragMove {
                center_px: (x - drag.grab_offset.0, y - drag.grab_offset.1),
            });
            return;
        }
        // A left press promoted to a drag on sufficient motion: the grab
        // anchors to the press position, so the element keeps the same
        // point under the pointer it had when the button went down.
        if let Some(press) = self.pressed {
            if matches!(press.button, PointerButton::Left) {
                let dx = x - press.pos.0;
                let dy = y - press.pos.1;
                if dx * dx + dy * dy > DRAG_THRESHOLD_PX * DRAG_THRESHOLD_PX {
                    self.dragging = Some(DragState {
                        element: press.element,
                        grab_offset: press.grab_offset,
                    });
                    events.push(InteractionEvent::DragStart {
                        element: press.element,
                        grab_offset: press.grab_offset,
                    });
                    events.push(InteractionEvent::DragMove {
                        center_px: (x - press.grab_offset.0, y - press.grab_offset.1),
                    });
                    return;
                }
            }
        }
        let hit = hit_test(hitboxes, x, y);
        if hit != self.hover {
            self.hover = hit;
            events.push(InteractionEvent::HoverChanged { element: hit });
        }
    }

    fn push_sample(&mut self, now: Instant, x: f64, y: f64) {
        self.samples.push_back((now, x, y));
        while let Some((t, _, _)) = self.samples.front() {
            if now.duration_since(*t) > VELOCITY_WINDOW {
                self.samples.pop_front();
            } else {
                break;
            }
        }
    }

    /// Pointer velocity over the sample window, logical px/s. Samples
    /// older than the window are pruned first, so releasing long after
    /// the pointer stopped moving yields (0, 0), not a stale flick.
    fn release_velocity(&self, now: Instant) -> (f64, f64) {
        let mut samples = self
            .samples
            .iter()
            .skip_while(|(t, _, _)| now.duration_since(*t) > VELOCITY_WINDOW)
            .copied();
        let (t0, x0, y0) = match samples.next() {
            Some(s) => s,
            None => return (0.0, 0.0),
        };
        let (t1, x1, y1) = match self.samples.back() {
            Some(s) => *s,
            None => return (0.0, 0.0),
        };
        let dt = t1.saturating_duration_since(t0).as_secs_f64();
        if dt < 1e-3 {
            (0.0, 0.0)
        } else {
            ((x1 - x0) / dt, (y1 - y0) / dt)
        }
    }
}

/// First hitbox (hit-priority order) containing the point.
pub fn hit_test(hitboxes: &[ElementHitbox], x: f64, y: f64) -> Option<ElementId> {
    hitboxes.iter().find(|h| h.contains(x, y)).map(|h| h.id)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect_hitboxes() -> Vec<ElementHitbox> {
        // Three nested elements in hit-priority order: eyes inside face
        // inside body (mirrors the prototype cube's ordering contract).
        vec![
            ElementHitbox {
                id: ElementId(5),
                rect: Rect {
                    x: 90,
                    y: 40,
                    width: 20,
                    height: 20,
                },
            },
            ElementHitbox {
                id: ElementId(4),
                rect: Rect {
                    x: 50,
                    y: 50,
                    width: 100,
                    height: 100,
                },
            },
            ElementHitbox {
                id: ElementId(1),
                rect: Rect {
                    x: 0,
                    y: 0,
                    width: 200,
                    height: 200,
                },
            },
        ]
    }

    fn motion(x: f64, y: f64) -> PointerEvent {
        PointerEvent {
            kind: PointerEventKind::Motion,
            x,
            y,
        }
    }

    fn enter(x: f64, y: f64) -> PointerEvent {
        PointerEvent {
            kind: PointerEventKind::Enter,
            x,
            y,
        }
    }

    fn leave() -> PointerEvent {
        PointerEvent {
            kind: PointerEventKind::Leave,
            x: 0.0,
            y: 0.0,
        }
    }

    fn press(button: PointerButton, x: f64, y: f64) -> PointerEvent {
        PointerEvent {
            kind: PointerEventKind::Press { button },
            x,
            y,
        }
    }

    fn release(button: PointerButton, x: f64, y: f64) -> PointerEvent {
        PointerEvent {
            kind: PointerEventKind::Release { button },
            x,
            y,
        }
    }

    fn at(ms: u64) -> Instant {
        Instant::now() + std::time::Duration::from_millis(ms)
    }

    #[test]
    fn hit_test_prefers_children_over_parents() {
        let hitboxes = rect_hitboxes();
        assert_eq!(hit_test(&hitboxes, 100.0, 50.0), Some(ElementId(5)));
        assert_eq!(hit_test(&hitboxes, 100.0, 120.0), Some(ElementId(4)));
        assert_eq!(hit_test(&hitboxes, 10.0, 10.0), Some(ElementId(1)));
        assert_eq!(hit_test(&hitboxes, 300.0, 300.0), None);
    }

    #[test]
    fn enter_and_leave_emit_hover_changes() {
        let mut system = InteractionSystem::new();
        let hitboxes = rect_hitboxes();

        let events = system.on_pointer(at(0), &enter(100.0, 50.0), &hitboxes);
        assert_eq!(
            events,
            vec![InteractionEvent::HoverChanged {
                element: Some(ElementId(5))
            }]
        );

        // Motion inside the same element emits nothing.
        assert!(system
            .on_pointer(at(10), &motion(95.0, 55.0), &hitboxes)
            .is_empty());

        let events = system.on_pointer(at(20), &motion(10.0, 10.0), &hitboxes);
        assert_eq!(
            events,
            vec![InteractionEvent::HoverChanged {
                element: Some(ElementId(1))
            }]
        );

        let events = system.on_pointer(at(30), &leave(), &hitboxes);
        assert_eq!(
            events,
            vec![InteractionEvent::HoverChanged { element: None }]
        );
        assert_eq!(system.hovered(), None);
    }

    #[test]
    fn press_and_release_without_motion_is_a_click() {
        let mut system = InteractionSystem::new();
        let hitboxes = rect_hitboxes();
        let _ = system.on_pointer(at(0), &enter(100.0, 120.0), &hitboxes);

        let events = system.on_pointer(at(5), &press(PointerButton::Left, 100.0, 120.0), &hitboxes);
        assert_eq!(
            events,
            vec![InteractionEvent::Press {
                element: ElementId(4),
                button: PointerButton::Left
            }]
        );

        // Sub-threshold wiggles stay a press and keep hover updates quiet.
        assert!(system
            .on_pointer(at(20), &motion(102.0, 121.0), &hitboxes)
            .is_empty());

        let events = system.on_pointer(
            at(40),
            &release(PointerButton::Left, 102.0, 121.0),
            &hitboxes,
        );
        assert_eq!(
            events,
            vec![InteractionEvent::Click {
                element: ElementId(4),
                button: PointerButton::Left
            }]
        );
    }

    #[test]
    fn motion_beyond_threshold_starts_a_drag_anchored_to_the_press() {
        let mut system = InteractionSystem::new();
        let hitboxes = rect_hitboxes();
        let _ = system.on_pointer(at(0), &press(PointerButton::Left, 100.0, 50.0), &hitboxes);

        // 10 px of motion promotes the press; the DragMove position keeps
        // the grab anchored (pointer minus press-time offset).
        let events = system.on_pointer(at(20), &motion(110.0, 50.0), &hitboxes);
        assert_eq!(
            events,
            vec![
                InteractionEvent::DragStart {
                    element: ElementId(5),
                    // Press at (100,50); eyes rect center is (100,50).
                    grab_offset: (0.0, 0.0)
                },
                InteractionEvent::DragMove {
                    center_px: (110.0, 50.0)
                },
            ]
        );
        assert!(system.is_dragging());

        // Further motion yields DragMove events; hover changes suppressed.
        let events = system.on_pointer(at(30), &motion(300.0, 300.0), &hitboxes);
        assert_eq!(
            events,
            vec![InteractionEvent::DragMove {
                center_px: (300.0, 300.0)
            }]
        );
    }

    #[test]
    fn release_velocity_is_sampled_from_recent_motion() {
        let mut system = InteractionSystem::new();
        let hitboxes = rect_hitboxes();
        let _ = system.on_pointer(at(0), &press(PointerButton::Left, 0.0, 0.0), &hitboxes);
        let _ = system.on_pointer(at(50), &motion(30.0, 0.0), &hitboxes);
        // 30 px over the 100 ms that remain inside the window = 300 px/s
        // (the press-time sample at t=0 aged out of the window).
        let _ = system.on_pointer(at(150), &motion(60.0, 0.0), &hitboxes);
        let events =
            system.on_pointer(at(160), &release(PointerButton::Left, 60.0, 0.0), &hitboxes);
        match events.first() {
            Some(InteractionEvent::DragEnd { velocity, .. }) => {
                assert!((velocity.0 - 300.0).abs() < 10.0, "{velocity:?}")
            }
            other => panic!("expected DragEnd, got {other:?}"),
        }
    }

    #[test]
    fn release_long_after_motion_stopped_has_zero_velocity() {
        let mut system = InteractionSystem::new();
        let hitboxes = rect_hitboxes();
        let _ = system.on_pointer(at(0), &press(PointerButton::Left, 5.0, 5.0), &hitboxes);
        let _ = system.on_pointer(at(5), &motion(50.0, 5.0), &hitboxes);
        // Everything older than the 120 ms window has aged out: the
        // pointer has been still for 195 ms, so there is no toss.
        let events =
            system.on_pointer(at(200), &release(PointerButton::Left, 50.0, 5.0), &hitboxes);
        match events.first() {
            Some(InteractionEvent::DragEnd { velocity, .. }) => {
                assert_eq!(*velocity, (0.0, 0.0))
            }
            other => panic!("expected DragEnd, got {other:?}"),
        }
    }

    #[test]
    fn release_recomputes_hover_at_the_drop_position() {
        let mut system = InteractionSystem::new();
        let hitboxes = rect_hitboxes();
        let _ = system.on_pointer(at(0), &press(PointerButton::Left, 100.0, 50.0), &hitboxes);
        let _ = system.on_pointer(at(10), &motion(110.0, 50.0), &hitboxes);

        // Drag ends with the pointer over the body element region.
        let events = system.on_pointer(
            at(40),
            &release(PointerButton::Left, 150.0, 150.0),
            &hitboxes,
        );
        assert_eq!(events.len(), 2);
        assert!(matches!(
            events[0],
            InteractionEvent::DragEnd {
                element: ElementId(5),
                ..
            }
        ));
        assert_eq!(
            events[1],
            InteractionEvent::HoverChanged {
                element: Some(ElementId(1))
            }
        );
        assert!(!system.is_dragging());
    }

    #[test]
    fn press_outside_every_element_is_ignored() {
        let mut system = InteractionSystem::new();
        let hitboxes = rect_hitboxes();
        let events = system.on_pointer(at(0), &press(PointerButton::Left, 500.0, 500.0), &hitboxes);
        assert!(events.is_empty());

        // A release without a matching press is also ignored.
        let events = system.on_pointer(
            at(10),
            &release(PointerButton::Left, 500.0, 500.0),
            &hitboxes,
        );
        assert!(events.is_empty());
    }

    #[test]
    fn leave_during_drag_keeps_the_drag_alive() {
        let mut system = InteractionSystem::new();
        let hitboxes = rect_hitboxes();
        let _ = system.on_pointer(at(0), &press(PointerButton::Left, 100.0, 50.0), &hitboxes);
        let _ = system.on_pointer(at(10), &motion(110.0, 50.0), &hitboxes);

        let events = system.on_pointer(at(20), &leave(), &hitboxes);
        assert!(events.is_empty());
        assert!(system.is_dragging());
    }

    #[test]
    fn leave_aborts_a_pending_press_that_never_dragged() {
        let mut system = InteractionSystem::new();
        let hitboxes = rect_hitboxes();
        let _ = system.on_pointer(at(0), &enter(100.0, 50.0), &hitboxes);
        let _ = system.on_pointer(at(5), &press(PointerButton::Left, 100.0, 50.0), &hitboxes);

        let events = system.on_pointer(at(10), &leave(), &hitboxes);
        assert_eq!(
            events,
            vec![InteractionEvent::HoverChanged { element: None }]
        );
        // The dangling press no longer produces a click on a later release.
        let events = system.on_pointer(
            at(20),
            &release(PointerButton::Left, 100.0, 50.0),
            &hitboxes,
        );
        assert!(events.is_empty());
    }

    #[test]
    fn release_of_a_different_button_does_not_click() {
        let mut system = InteractionSystem::new();
        let hitboxes = rect_hitboxes();
        let _ = system.on_pointer(at(0), &press(PointerButton::Left, 100.0, 50.0), &hitboxes);
        let events = system.on_pointer(
            at(10),
            &release(PointerButton::Right, 100.0, 50.0),
            &hitboxes,
        );
        assert!(events.is_empty());
    }
}
