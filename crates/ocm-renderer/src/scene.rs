//! Camera and character placement math.
//!
//! The character is placed on a **screen-parallel anchor plane**: for a
//! normalized position `(u, v)` we cast a ray through the corresponding NDC
//! point and intersect it with a plane perpendicular to the view direction
//! passing through a reference point. This guarantees:
//!
//! * position (u, v) maps to screen pixel (u·W, v·H) *exactly* — so the
//!   input-region hitbox is a trivial screen-space rectangle;
//! * the character's distance from the camera is constant, so its apparent
//!   size is constant at every position.

use crate::math::{self, Mat4, Vec3};
use crate::SceneState;

/// Fixed camera parameters. A slight downward tilt makes the cube read as
/// 3D while keeping the anchor-plane math exact.
const EYE: Vec3 = [0.0, 2.6, 5.4];
const TARGET: Vec3 = [0.0, 0.55, 0.0];
const UP: Vec3 = [0.0, 1.0, 0.0];
const FOV_Y: f32 = 30.0_f32.to_radians();
const NEAR: f32 = 0.1;
const FAR: f32 = 100.0;

/// A point on the anchor plane, and its screen-parallel placement data.
#[derive(Debug, Clone, Copy)]
pub struct Placement {
    pub view_proj: Mat4,
    /// World position of the character anchor.
    pub center: Vec3,
    /// Camera basis vectors (right, up, forward).
    pub right: Vec3,
    pub up: Vec3,
    pub forward: Vec3,
    /// Character world height (cube spans exactly this).
    pub world_h: f32,
    /// Hitbox center in logical pixels (top-left origin) and radius.
    pub hitbox_center_px: (f32, f32),
    pub hitbox_radius_px: f32,
}

/// Camera: view-projection matrix plus basis, parameterized by the logical
/// surface size (for aspect and pixel mapping).
pub struct Camera {
    view_proj: Mat4,
    right: Vec3,
    up: Vec3,
    forward: Vec3,
    /// Constant distance from the eye to the anchor plane.
    anchor_distance: f32,
    logical: (u32, u32),
}

impl Camera {
    pub fn new(logical: (u32, u32)) -> Self {
        let aspect = logical.0 as f32 / logical.1.max(1) as f32;
        let proj = math::perspective_zo(aspect, FOV_Y, NEAR, FAR);
        let view = math::look_at(EYE, TARGET, UP);
        let view_proj = math::mat_mul(&proj, &view);

        let forward = math::normalize(math::sub(TARGET, EYE));
        let right = math::normalize(math::cross(forward, UP));
        let up = math::cross(right, forward);

        // Distance from the eye to the anchor plane (plane through ANCHOR_REF
        // perpendicular to the view direction):
        // t = f·(ref − eye) / (f·dir), and f·dir = f·f = 1 for our ray
        // construction (dir = f + right·x + up·y with orthogonal basis).
        let anchor_ref: Vec3 = [0.0, 0.55, 0.0];
        let anchor_distance = math::dot(
            forward,
            math::sub(anchor_ref, EYE),
        );

        Self { view_proj, right, up, forward, anchor_distance, logical }
    }

    /// Compute the full placement for a scene snapshot.
    pub fn placement(&self, scene: &SceneState, logical: (u32, u32)) -> Placement {
        // NDC point for the normalized position (top-left origin in input,
        // NDC y points up).
        let ndc_x = scene.position.0 as f32 * 2.0 - 1.0;
        let ndc_y = 1.0 - scene.position.1 as f32 * 2.0;

        let aspect = self.logical.0 as f32 / self.logical.1.max(1) as f32;
        let tan_half = (FOV_Y / 2.0).tan();

        // Ray through the NDC point, in world space.
        let dir = math::normalize([
            self.forward[0]
                + self.right[0] * ndc_x * tan_half * aspect
                + self.up[0] * ndc_y * tan_half,
            self.forward[1]
                + self.right[1] * ndc_x * tan_half * aspect
                + self.up[1] * ndc_y * tan_half,
            self.forward[2]
                + self.right[2] * ndc_x * tan_half * aspect
                + self.up[2] * ndc_y * tan_half,
        ]);

        // Intersect with the anchor plane (perpendicular to view, through
        // anchor_distance): t = anchor_distance / (f · dir).
        let denom = math::dot(self.forward, dir);
        let t = if denom.abs() < 1e-6 {
            self.anchor_distance
        } else {
            self.anchor_distance / denom
        };
        let center: Vec3 = [
            EYE[0] + dir[0] * t,
            EYE[1] + dir[1] * t,
            EYE[2] + dir[2] * t,
        ];

        // World height for the wanted on-screen fraction. At the anchor
        // distance, one logical pixel measures:
        // px_per_world = H / (2 · tan(fov/2) · anchor_distance)
        let h_logical = self.logical.1 as f32;
        let px_per_world = h_logical / (2.0 * tan_half * self.anchor_distance.max(0.1));
        let world_h = (scene.size_frac.max(0.02) * h_logical / px_per_world).max(0.02);

        // Hitbox in logical pixels: the anchor maps to exactly
        // (u·W, v·H); the cube's worst-case screen half-extent is its
        // half-diagonal (yaw rotation), plus a small grab margin.
        let half = world_h / 2.0;
        let radius_world = half * std::f32::consts::SQRT_2 * 1.12;
        let hitbox_radius_px = radius_world * px_per_world;
        let hitbox_center_px = (
            scene.position.0 as f32 * self.logical.0 as f32,
            scene.position.1 as f32 * self.logical.1 as f32,
        );

        let _ = logical; // camera is already framed for the right size

        Placement {
            view_proj: self.view_proj,
            center,
            right: self.right,
            up: self.up,
            forward: self.forward,
            world_h,
            hitbox_center_px,
            hitbox_radius_px,
        }
    }
}
