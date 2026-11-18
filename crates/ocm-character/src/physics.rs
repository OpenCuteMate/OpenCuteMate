//! Deterministic character physics: gravity, velocity, falling and
//! collision with the output bounds.
//!
//! Deliberately tiny and pure — no Rapier, no broadphase, no shapes beyond
//! the character's axis-aligned box. Everything is a function of
//! `(position, velocity, dt, bounds)`, so identical inputs yield bit-equal
//! outputs (see the determinism tests). The runtime steps physics only
//! while the body is unsettled; a resting character parks with the rest of
//! the app.
//!
//! Units are **logical pixels** and seconds; screen-space y grows downward
//! (matching Wayland surface coordinates), so gravity is +y.

use crate::desktop::DesktopBounds;

/// Gravity, in output-heights per s². A character dropped from the top of
/// the screen lands in ~0.8 s.
pub const GRAVITY_HEIGHTS_PER_S2: f64 = 3.2;
/// Terminal velocity, in output-heights per s.
pub const TERMINAL_HEIGHTS_PER_S: f64 = 4.0;
/// Velocity fraction kept after bouncing off a wall / the ceiling.
const WALL_RESTITUTION: f64 = 0.45;
/// Velocity fraction kept after a hard landing (small hops only).
const GROUND_RESTITUTION: f64 = 0.28;
/// Landings slower than this (output-heights per s) do not bounce.
const BOUNCE_THRESHOLD_HEIGHTS: f64 = 0.18;
/// Ground deceleration (constant), in output-heights per s². A 600 px/s
/// release slides ~100 px; a hard toss skitters between walls.
const GROUND_DECEL_HEIGHTS_PER_S2: f64 = 2.0;
/// Horizontal speed below which a grounded body is considered at rest.
const REST_EPS_PX: f64 = 8.0;
/// Upper bound on one integration step: a park or compositor stall must
/// never teleport the character through a boundary.
pub const MAX_STEP: f32 = 0.05;

/// Observable outcomes of one physics step.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PhysicsEvent {
    /// The body touched the ground and stopped bouncing.
    Landed,
    /// A bounce off the ground (still airborne afterwards).
    BouncedGround,
    /// Hit a side wall (or the ceiling).
    HitWall,
    /// The grounded body slid to a rest.
    Rest,
}

/// State carried between steps.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Body {
    /// Character anchor in logical px (surface coordinates, top-left
    /// origin).
    pub position: (f64, f64),
    /// Anchor velocity in logical px/s.
    pub velocity: (f64, f64),
    /// Airborne = gravity applies; grounded = friction on the floor.
    pub airborne: bool,
}

impl Body {
    /// One semi-implicit Euler step against the bounds. `half_extent` is
    /// the character's physical half-size in logical px (the body box; side
    /// elements are ignored as a documented simplification).
    pub fn step(
        &mut self,
        dt: f32,
        bounds: &DesktopBounds,
        half_extent: (f64, f64),
    ) -> Vec<PhysicsEvent> {
        let mut events = Vec::new();
        let dt = dt.min(MAX_STEP) as f64;
        if dt <= 0.0 {
            return events;
        }
        let (w, h) = (bounds.width as f64, bounds.height as f64);

        if self.airborne {
            let gravity = GRAVITY_HEIGHTS_PER_S2 * h;
            let terminal = TERMINAL_HEIGHTS_PER_S * h;
            self.velocity.1 = (self.velocity.1 + gravity * dt).min(terminal);
            self.position.0 += self.velocity.0 * dt;
            self.position.1 += self.velocity.1 * dt;

            // Side walls and ceiling: clamp + reflect.
            let hit_wall = if self.position.0 < half_extent.0 {
                self.position.0 = half_extent.0;
                self.reflect_x();
                true
            } else if self.position.0 > w - half_extent.0 {
                self.position.0 = w - half_extent.0;
                self.reflect_x();
                true
            } else if self.position.1 < half_extent.1 {
                self.position.1 = half_extent.1;
                self.velocity.1 = -self.velocity.1 * WALL_RESTITUTION;
                true
            } else {
                false
            };
            if hit_wall {
                events.push(PhysicsEvent::HitWall);
            }

            // Ground: land or bounce.
            if self.position.1 >= h - half_extent.1 {
                self.position.1 = h - half_extent.1;
                let landing_speed = self.velocity.1;
                if landing_speed > BOUNCE_THRESHOLD_HEIGHTS * h {
                    self.velocity.1 = -landing_speed * GROUND_RESTITUTION;
                    events.push(PhysicsEvent::BouncedGround);
                } else {
                    self.velocity.1 = 0.0;
                    self.airborne = false;
                    events.push(PhysicsEvent::Landed);
                }
            }
        } else if self.velocity.0.abs() > REST_EPS_PX {
            // Grounded slide: constant deceleration (quadratic stopping
            // distance — hard tosses travel far, gentle releases stick),
            // with wall reflection while sliding.
            self.position.0 += self.velocity.0 * dt;
            let clamped = self.position.0.clamp(half_extent.0, w - half_extent.0);
            if clamped != self.position.0 {
                self.position.0 = clamped;
                self.reflect_x();
                events.push(PhysicsEvent::HitWall);
            }
            let decel = GROUND_DECEL_HEIGHTS_PER_S2 * h * dt;
            let magnitude = self.velocity.0.abs();
            self.velocity.0 = if magnitude <= decel {
                0.0
            } else {
                self.velocity.0 - self.velocity.0.signum() * decel
            };
            if self.velocity.0.abs() <= REST_EPS_PX {
                self.velocity.0 = 0.0;
                events.push(PhysicsEvent::Rest);
            }
        }
        events
    }

    fn reflect_x(&mut self) {
        if self.velocity.0.abs() > REST_EPS_PX {
            self.velocity.0 = -self.velocity.0 * WALL_RESTITUTION;
        } else {
            self.velocity.0 = 0.0;
        }
    }

    /// Whether the body has fully settled (no further stepping needed).
    pub fn settled(&self) -> bool {
        !self.airborne && self.velocity.0.abs() <= REST_EPS_PX
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BOUNDS: DesktopBounds = DesktopBounds {
        width: 1600,
        height: 900,
    };
    const HALF: (f64, f64) = (180.0, 180.0); // size_frac 0.4 on a 900 px output

    fn grounded_body() -> Body {
        Body {
            position: (800.0, 900.0 - 180.0),
            velocity: (0.0, 0.0),
            airborne: false,
        }
    }

    #[test]
    fn falling_body_lands_exactly_on_the_ground() {
        let mut body = Body {
            position: (800.0, 100.0),
            velocity: (0.0, 0.0),
            airborne: true,
        };
        let mut landed = false;
        for _ in 0..200 {
            let events = body.step(1.0 / 60.0, &BOUNDS, HALF);
            if events.contains(&PhysicsEvent::Landed) {
                landed = true;
                break;
            }
        }
        assert!(landed, "never landed");
        assert_eq!(body.position.1, BOUNDS.height as f64 - HALF.1);
        assert!(body.settled());
        assert!(!body.airborne);
    }

    #[test]
    fn physics_is_deterministic_for_identical_input_sequences() {
        let step_sequence = |mut body: Body| -> Vec<(f64, f64, f64, f64)> {
            let mut trace = Vec::new();
            for i in 0..120 {
                let dt = 1.0 / 60.0 + (i % 7) as f32 * 0.001;
                let _ = body.step(dt, &BOUNDS, HALF);
                trace.push((
                    body.position.0,
                    body.position.1,
                    body.velocity.0,
                    body.velocity.1,
                ));
            }
            trace
        };
        let a = step_sequence(Body {
            position: (400.0, 100.0),
            velocity: (500.0, -300.0),
            airborne: true,
        });
        let b = step_sequence(Body {
            position: (400.0, 100.0),
            velocity: (500.0, -300.0),
            airborne: true,
        });
        assert_eq!(a, b, "identical inputs must produce bit-equal traces");
    }

    #[test]
    fn toss_is_parabolic_until_the_ground() {
        // vy0 = -360 px/s (upward), g = 3.2 * 900 = 2880 px/s².
        let mut body = Body {
            position: (800.0, 600.0),
            velocity: (0.0, -360.0),
            airborne: true,
        };
        // Analytic apex: t = 360/2880 = 0.125 s, rise = 360² / (2·2880) = 22.5 px.
        let mut max_rise: f64 = 0.0;
        for _ in 0..15 {
            let _ = body.step(1.0 / 60.0, &BOUNDS, HALF);
            max_rise = max_rise.max(600.0 - body.position.1);
        }
        // One step of integration lag; within a per-step velocity error.
        assert!(
            (max_rise - 22.5).abs() < 4.0,
            "apex rise {max_rise} vs analytic 22.5"
        );
        assert!(body.airborne, "0.25 s is before the parabola returns");
    }

    #[test]
    fn horizontal_toss_bounces_off_walls_and_settles() {
        let mut body = Body {
            position: (800.0, 720.0),
            velocity: (5000.0, 0.0),
            airborne: true,
        };
        let mut wall_hits = 0;
        let mut steps = 0;
        while !body.settled() && steps < 3000 {
            let events = body.step(1.0 / 60.0, &BOUNDS, HALF);
            wall_hits += events
                .iter()
                .filter(|e| **e == PhysicsEvent::HitWall)
                .count();
            steps += 1;
        }
        assert!(body.settled(), "never settled after {steps} steps");
        assert!(
            wall_hits >= 2,
            "a 5000 px/s toss must reach both walls, hit {wall_hits}"
        );
        let margin = BOUNDS.width as f64 - HALF.0;
        assert!(
            (HALF.0..=margin).contains(&body.position.0),
            "resting x {} out of bounds",
            body.position.0
        );
    }

    #[test]
    fn slow_landing_does_not_bounce() {
        // Impact speed ≈ 148 px/s < the 162 px/s bounce threshold.
        let mut body = Body {
            position: (800.0, 719.0),
            velocity: (0.0, 100.0),
            airborne: true,
        };
        let mut events = Vec::new();
        for _ in 0..200 {
            events = body.step(1.0 / 60.0, &BOUNDS, HALF);
            if !events.is_empty() {
                break;
            }
        }
        assert_eq!(events, vec![PhysicsEvent::Landed]);
    }

    #[test]
    fn fast_landing_bounces_then_lands() {
        let mut body = Body {
            position: (800.0, 100.0),
            velocity: (0.0, 2500.0),
            airborne: true,
        };
        let mut saw_bounce = false;
        for _ in 0..400 {
            let events = body.step(1.0 / 60.0, &BOUNDS, HALF);
            if events.contains(&PhysicsEvent::BouncedGround) {
                saw_bounce = true;
            }
            if events.contains(&PhysicsEvent::Landed) {
                break;
            }
        }
        assert!(saw_bounce, "a hard landing should produce one hop");
        assert!(body.settled());
    }

    #[test]
    fn grounded_slide_decays_to_rest() {
        let mut body = grounded_body();
        body.velocity.0 = 600.0;
        let mut rested = false;
        for _ in 0..240 {
            let events = body.step(1.0 / 60.0, &BOUNDS, HALF);
            if events.contains(&PhysicsEvent::Rest) {
                rested = true;
                break;
            }
        }
        assert!(rested, "never came to rest");
        assert_eq!(body.velocity.0, 0.0);
        assert!(body.settled());
        // Constant deceleration: 600 px/s stops within v²/(2k) ≈ 100 px.
        assert!((body.position.0 - 800.0).abs() < 120.0);
    }

    #[test]
    fn a_settled_body_produces_no_events() {
        let mut body = grounded_body();
        let events = body.step(1.0 / 60.0, &BOUNDS, HALF);
        assert!(events.is_empty());
        assert!(body.settled());
    }

    #[test]
    fn huge_dt_is_clamped_and_cannot_tunnel_through_the_ground() {
        let mut body = Body {
            position: (800.0, 0.0),
            velocity: (0.0, 0.0),
            airborne: true,
        };
        let events = body.step(10.0, &BOUNDS, HALF);
        // dt clamps to MAX_STEP; one step alone cannot cross the output.
        assert!(!events.contains(&PhysicsEvent::Landed));
        assert!(body.position.1 < BOUNDS.height as f64);
    }

    #[test]
    fn ceiling_toss_reflects_downward() {
        let mut body = Body {
            position: (800.0, 300.0),
            velocity: (0.0, -3000.0),
            airborne: true,
        };
        let mut hit_ceiling = false;
        for _ in 0..30 {
            let events = body.step(1.0 / 60.0, &BOUNDS, HALF);
            if events.contains(&PhysicsEvent::HitWall) && body.position.1 <= HALF.1 + 1.0 {
                hit_ceiling = true;
                break;
            }
        }
        assert!(hit_ceiling, "the upward toss never reached the ceiling");
        assert_eq!(body.position.1, HALF.1);
        assert!(body.velocity.1 > 0.0, "reflected downward");
    }
}
