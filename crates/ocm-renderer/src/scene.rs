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
use ocm_character::model::CharacterModel;
use ocm_character::{ElementHitbox, ElementId};
use ocm_platform::Rect;

/// Fixed camera parameters. A slight downward tilt makes the cube read as
/// 3D while keeping the anchor-plane math exact.
pub(crate) const EYE: Vec3 = [0.0, 2.6, 5.4];
pub(crate) const TARGET: Vec3 = [0.0, 0.55, 0.0];
const UP: Vec3 = [0.0, 1.0, 0.0];
pub(crate) const FOV_Y: f32 = 30.0_f32.to_radians();
pub(crate) const NEAR: f32 = 0.1;
pub(crate) const FAR: f32 = 100.0;

/// Worst-case hitbox inflation: a yawed cube's screen half-extent is its
/// half-diagonal (√2), times a small grab margin (1.12) — the same reach
/// the Phase 2 PoC granted its cube.
pub(crate) const HIT_MARGIN: f32 = std::f32::consts::SQRT_2 * 1.12;

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
    /// Character world height (the model spans exactly this).
    pub world_h: f32,
    /// Logical pixels per world unit at the anchor distance.
    pub px_per_world: f32,
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
        let anchor_distance = math::dot(forward, math::sub(anchor_ref, EYE));

        Self {
            view_proj,
            right,
            up,
            forward,
            anchor_distance,
            logical,
        }
    }

    /// Compute the full placement for a scene snapshot.
    pub fn placement(&self, scene: &SceneState) -> Placement {
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

        Placement {
            view_proj: self.view_proj,
            center,
            right: self.right,
            up: self.up,
            forward: self.forward,
            world_h,
            px_per_world,
            hitbox_center_px,
            hitbox_radius_px,
        }
    }
}

/// Per-element hitboxes in logical pixels for a scene, in hit-priority
/// order (children before parents — mirrors
/// `CharacterModel::hit_priority`).
///
/// Rect centers sit exactly at each element's model offset from the
/// anchor: `anchor_px + (ox·h, −oy·h)`. The interaction system relies on
/// this to map element-space drag positions back to the anchor.
pub fn element_hitboxes(
    scene: &SceneState,
    model: &CharacterModel,
    camera: &Camera,
) -> Vec<ElementHitbox> {
    let placement = camera.placement(scene);
    let h_px = placement.world_h * placement.px_per_world;
    let (ax, ay) = placement.hitbox_center_px;
    model
        .hit_priority()
        .map(|element| {
            let extent = element.hit_extent.expect("hit_priority yields interactive");
            let cx = ax + element.offset[0] * h_px;
            let cy = ay - element.offset[1] * h_px;
            let rx = extent[0] * h_px * HIT_MARGIN;
            let ry = extent[1] * h_px * HIT_MARGIN;
            ElementHitbox {
                id: ElementId(element.id.0),
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

/// Root model matrix: anchor placement + the pose's root transform
/// (offset along the camera basis, yaw/pitch/roll, squash scale).
///
/// With a neutral pose this reduces exactly to the Phase 2 formula
/// `translate(center) · rotate_y(0) · scale(world_h)`.
pub fn root_model(placement: &Placement, root: &ocm_character::RootPose) -> Mat4 {
    let h = placement.world_h;
    let offset: math::Vec3 = [
        (placement.right[0] * root.offset_right + placement.up[0] * root.offset_up) * h,
        (placement.right[1] * root.offset_right + placement.up[1] * root.offset_up) * h,
        (placement.right[2] * root.offset_right + placement.up[2] * root.offset_up) * h,
    ];
    let translate = math::translate(
        placement.center[0] + offset[0],
        placement.center[1] + offset[1],
        placement.center[2] + offset[2],
    );
    let rotation = math::mat_mul(
        &math::rotate_y(root.yaw),
        &math::mat_mul(&math::rotate_x(root.pitch), &math::rotate_z(root.roll)),
    );
    let scale = math::scale3(
        h * root.scale[0].max(0.01),
        h * root.scale[1].max(0.01),
        h * root.scale[2].max(0.01),
    );
    math::mat_mul(&translate, &math::mat_mul(&rotation, &scale))
}

/// One part's model matrix: root · element · (element pose) · part.
pub fn part_model(
    root: &Mat4,
    element: &ocm_character::model::ModelElement,
    element_pose: &ocm_character::ElementPose,
    part: &ocm_character::model::VisualPart,
) -> Mat4 {
    let element_transform = math::mat_mul(
        &math::translate(element.offset[0], element.offset[1], element.offset[2]),
        &math::scale3(
            element_pose.scale[0].max(0.01),
            element_pose.scale[1].max(0.01),
            element_pose.scale[2].max(0.01),
        ),
    );
    let part_transform = math::mat_mul(
        &math::translate(part.offset[0], part.offset[1], part.offset[2]),
        &math::scale3(
            part.size[0].max(0.01),
            part.size[1].max(0.01),
            part.size[2].max(0.01),
        ),
    );
    math::mat_mul(root, &math::mat_mul(&element_transform, &part_transform))
}

/// The soft shadow billboard below the character, eased by the pose's
/// vertical offset (higher character = weaker, lower shadow).
pub fn shadow_model(placement: &Placement, root: &ocm_character::RootPose) -> Mat4 {
    let h = placement.world_h;
    let lift = root.offset_up * h;
    let sink = 0.72 * h + lift * 0.5;
    math::billboard(
        [
            placement.center[0] - placement.up[0] * sink,
            placement.center[1] - placement.up[1] * sink,
            placement.center[2] - placement.up[2] * sink,
        ],
        [
            placement.right[0] * h * 0.95,
            placement.right[1] * h * 0.95,
            placement.right[2] * h * 0.95,
        ],
        [
            placement.up[0] * h * 0.30,
            placement.up[1] * h * 0.30,
            placement.up[2] * h * 0.30,
        ],
        placement.forward,
    )
}

/// Shadow strength: fades as the character rises on its bob.
pub fn shadow_strength(root: &ocm_character::RootPose) -> f32 {
    0.42 * (1.0 - (root.offset_up / 0.06).abs().clamp(0.0, 1.0) * 0.45)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ocm_character::prototype_cube;

    fn scene(u: f64, v: f64, size_frac: f32) -> SceneState {
        SceneState {
            position: (u, v),
            size_frac,
            hover: None,
            pose: ocm_character::AnimatedPose::neutral(6),
        }
    }

    /// The anchor-plane contract: the character anchor maps to exactly
    /// (u·W, v·H) logical pixels, so the input-region hitbox always covers
    /// what is drawn on screen.
    #[test]
    fn hitbox_center_maps_position_exactly() {
        for (w, h) in [(1536, 864), (1920, 1080)] {
            let camera = Camera::new((w, h));
            for (u, v) in [(0.5, 0.55), (0.22, 0.30), (0.0, 1.0), (1.0, 0.0)] {
                let p = camera.placement(&scene(u, v, 0.4));
                assert_eq!(p.hitbox_center_px.0, u as f32 * w as f32);
                assert_eq!(p.hitbox_center_px.1, v as f32 * h as f32);
            }
        }
    }

    /// The anchor always lies on the screen-parallel plane at the fixed
    /// camera distance — the invariant that makes the pixel mapping exact.
    #[test]
    fn anchor_lies_on_the_anchor_plane() {
        let camera = Camera::new((1536, 864));
        for (u, v) in [(0.1, 0.1), (0.5, 0.5), (0.9, 0.8)] {
            let p = camera.placement(&scene(u, v, 0.4));
            let d = math::dot(math::sub(p.center, EYE), camera.forward);
            assert!((d - camera.anchor_distance).abs() < 1e-3, "d = {d}");
        }
    }

    #[test]
    fn hitbox_radius_scales_with_character_size() {
        let camera = Camera::new((1536, 864));
        let small = camera.placement(&scene(0.5, 0.5, 0.1)).hitbox_radius_px;
        let large = camera.placement(&scene(0.5, 0.5, 0.8)).hitbox_radius_px;
        assert!(small > 0.0);
        assert!((large / small - 8.0).abs() < 1e-3);
    }

    #[test]
    fn world_height_scales_with_character_size() {
        let camera = Camera::new((1536, 864));
        let small = camera.placement(&scene(0.5, 0.5, 0.1)).world_h;
        let large = camera.placement(&scene(0.5, 0.5, 0.2)).world_h;
        assert!((large / small - 2.0).abs() < 1e-3);
    }

    /// The per-element hitboxes: priority order, rect centers exactly at
    /// the model offsets, and the body matching the legacy PoC grab rect.
    #[test]
    fn element_hitboxes_follow_the_model() {
        let model = prototype_cube();
        let camera = Camera::new((1536, 864));
        let hitboxes = element_hitboxes(&scene(0.5, 0.55, 0.4), &model, &camera);

        // Hit-priority order: eyes first, body last; root excluded.
        assert_eq!(hitboxes[0].id, ElementId(5));
        assert_eq!(hitboxes[1].id, ElementId(4));
        assert_eq!(hitboxes.last().unwrap().id, ElementId(1));
        assert!(!hitboxes.iter().any(|h| h.id == ElementId(0)));

        // The anchor (center of the cube) lies on the face element, not
        // the eyes (their rect must stay above the anchor).
        let anchor = (0.5 * 1536.0, 0.55 * 864.0);
        let eyes = &hitboxes[0];
        assert!(!eyes.contains(anchor.0, anchor.1));
        let face = &hitboxes[1];
        assert!(face.contains(anchor.0, anchor.1));
    }

    #[test]
    fn body_hitbox_matches_the_legacy_poc_reach() {
        let model = prototype_cube();
        let camera = Camera::new((1536, 864));
        let scene = scene(0.5, 0.55, 0.4);
        let placement = camera.placement(&scene);
        let body = element_hitboxes(&scene, &model, &camera)
            .into_iter()
            .find(|h| h.id == ElementId(1))
            .expect("body hitbox");

        let legacy_half = placement.hitbox_radius_px;
        let center_x = body.rect.x as f32 + body.rect.width as f32 / 2.0;
        let half_w = body.rect.width as f32 / 2.0;
        // Same square reach as the Phase 2 input region (±1 px rounding).
        assert!((center_x - 0.5 * 1536.0).abs() < 1.0);
        assert!(
            (half_w - legacy_half).abs() < 1.5,
            "{half_w} vs {legacy_half}"
        );
    }

    /// A moving pose (bob/squash) must not move the hitboxes — the input
    /// region is compositor state and must not churn on animations.
    #[test]
    fn hitboxes_are_pose_independent() {
        let model = prototype_cube();
        let camera = Camera::new((1536, 864));
        let mut posed = scene(0.5, 0.55, 0.4);
        posed.pose.root.offset_up = 0.3;
        posed.pose.root.scale = [1.2, 0.7, 1.2];

        let plain = element_hitboxes(&scene(0.5, 0.55, 0.4), &model, &camera);
        let moved = element_hitboxes(&posed, &model, &camera);
        assert_eq!(plain, moved);
    }
}
