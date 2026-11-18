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
    scale3(s, s, s)
}

/// Non-uniform scale (squash & stretch).
pub fn scale3(sx: f32, sy: f32, sz: f32) -> Mat4 {
    [
        [sx, 0.0, 0.0, 0.0],
        [0.0, sy, 0.0, 0.0],
        [0.0, 0.0, sz, 0.0],
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

/// Rotation around the world X axis (screen-space pitch for the camera
/// looking down -Z with +X right).
pub fn rotate_x(angle: f32) -> Mat4 {
    let (sin, cos) = angle.sin_cos();
    [
        [1.0, 0.0, 0.0, 0.0],
        [0.0, cos, sin, 0.0],
        [0.0, -sin, cos, 0.0],
        [0.0, 0.0, 0.0, 1.0],
    ]
}

/// Rotation around the world Z axis (screen-space roll; positive angles
/// appear counterclockwise to the viewer).
pub fn rotate_z(angle: f32) -> Mat4 {
    let (sin, cos) = angle.sin_cos();
    [
        [cos, sin, 0.0, 0.0],
        [-sin, cos, 0.0, 0.0],
        [0.0, 0.0, 1.0, 0.0],
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

#[cfg(test)]
mod tests {
    use super::*;

    fn apply(m: Mat4, v: Vec3) -> Vec3 {
        let p = [v[0], v[1], v[2], 1.0];
        let mut out = [0.0f32; 4];
        for r in 0..4 {
            let mut sum = 0.0;
            for k in 0..4 {
                sum += m[k][r] * p[k];
            }
            out[r] = sum;
        }
        [out[0], out[1], out[2]]
    }

    #[test]
    fn rotate_y_turns_the_front_face_left() {
        // +Z rotated by +90° around Y lands on +X... for the
        // right-handed convention here: [sin, 0, cos] columns.
        let v = apply(rotate_y(std::f32::consts::FRAC_PI_2), [0.0, 0.0, 1.0]);
        assert!((v[0] - 1.0).abs() < 1e-6, "{v:?}");
        assert!(v[1].abs() < 1e-6 && v[2].abs() < 1e-6);
    }

    #[test]
    fn rotate_x_pitches_the_nose_down() {
        // +Z rotated by +90° around X lands on -Y (nose-down for our
        // screen-space pitch).
        let v = apply(rotate_x(std::f32::consts::FRAC_PI_2), [0.0, 0.0, 1.0]);
        assert!((v[1] + 1.0).abs() < 1e-6, "{v:?}");
        assert!(v[0].abs() < 1e-6 && v[2].abs() < 1e-6);
    }

    #[test]
    fn rotate_z_is_counterclockwise_to_the_viewer() {
        // +X rolled by +90° around Z (the view axis) points up (+Y).
        let v = apply(rotate_z(std::f32::consts::FRAC_PI_2), [1.0, 0.0, 0.0]);
        assert!((v[1] - 1.0).abs() < 1e-6, "{v:?}");
        assert!(v[0].abs() < 1e-6 && v[2].abs() < 1e-6);
    }
}
