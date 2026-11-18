//! Minimal hand-rolled 4x4 matrix math (column-major, matching WGSL's
//! mat4x4 memory layout). No external math dependency by design: the PoC
//! needs exactly these six operations, and keeping the workspace dependency
//! list minimal is an explicit project constraint.

/// Column-major 4x4 matrix: `m[col][row]`.
pub type Mat4 = [[f32; 4]; 4];

/// `a * b` (applies `b` first in the transform chain).
pub fn mat_mul(a: &Mat4, b: &Mat4) -> Mat4 {
    let mut out = [[0.0f32; 4]; 4];
    for c in 0..4 {
        for r in 0..4 {
            let mut sum = 0.0;
            for k in 0..4 {
                sum += a[k][r] * b[c][k];
            }
            out[c][r] = sum;
        }
    }
    out
}

pub fn translate(tx: f32, ty: f32, tz: f32) -> Mat4 {
    [
        [1.0, 0.0, 0.0, 0.0],
        [0.0, 1.0, 0.0, 0.0],
        [0.0, 0.0, 1.0, 0.0],
        [tx, ty, tz, 1.0],
    ]
}

pub fn scale(s: f32) -> Mat4 {
    [
        [s, 0.0, 0.0, 0.0],
        [0.0, s, 0.0, 0.0],
        [0.0, 0.0, s, 0.0],
        [0.0, 0.0, 0.0, 1.0],
    ]
}

/// Rotation around the world Y axis.
pub fn rotate_y(angle: f32) -> Mat4 {
    let (sin, cos) = angle.sin_cos();
    [
        [cos, 0.0, -sin, 0.0],
        [0.0, 1.0, 0.0, 0.0],
        [sin, 0.0, cos, 0.0],
        [0.0, 0.0, 0.0, 1.0],
    ]
}

/// Right-handed perspective with a zero-to-one depth range (Vulkan NDC).
/// `fov_y` in radians.
pub fn perspective_zo(aspect: f32, fov_y: f32, near: f32, far: f32) -> Mat4 {
    let f = 1.0 / (fov_y / 2.0).tan();
    [
        [f / aspect, 0.0, 0.0, 0.0],
        [0.0, f, 0.0, 0.0],
        [0.0, 0.0, far / (near - far), -1.0],
        [0.0, 0.0, (near * far) / (near - far), 0.0],
    ]
}

/// Right-handed look-at view matrix.
pub fn look_at(eye: [f32; 3], target: [f32; 3], up: [f32; 3]) -> Mat4 {
    let f = normalize(sub(target, eye)); // forward
    let r = normalize(cross(f, up)); // right
    let u = cross(r, f); // camera up
    [
        [r[0], u[0], -f[0], 0.0],
        [r[1], u[1], -f[1], 0.0],
        [r[2], u[2], -f[2], 0.0],
        [-dot(r, eye), -dot(u, eye), dot(f, eye), 1.0],
    ]
}

/// Billboard matrix: quad local X → `axis_x`, local Y → `axis_y`, local Z →
/// `forward`, origin at `center`.
pub fn billboard(center: [f32; 3], axis_x: [f32; 3], axis_y: [f32; 3], forward: [f32; 3]) -> Mat4 {
    [
        [axis_x[0], axis_x[1], axis_x[2], 0.0],
        [axis_y[0], axis_y[1], axis_y[2], 0.0],
        [forward[0], forward[1], forward[2], 0.0],
        [center[0], center[1], center[2], 1.0],
    ]
}

// --- small vec3 helpers -------------------------------------------------

pub type Vec3 = [f32; 3];

pub fn sub(a: Vec3, b: Vec3) -> Vec3 {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

pub fn dot(a: Vec3, b: Vec3) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

pub fn cross(a: Vec3, b: Vec3) -> Vec3 {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

pub fn normalize(v: Vec3) -> Vec3 {
    let len = dot(v, v).sqrt();
    if len < f32::EPSILON {
        return [0.0, 0.0, 0.0];
    }
    [v[0] / len, v[1] / len, v[2] / len]
}
