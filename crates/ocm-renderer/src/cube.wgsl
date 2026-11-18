// OpenCuteMate PoC shader: procedural cube + soft billboard shadow over a
// transparent background. Alpha-mode aware via u.params.x.

struct Uniforms {
    view_proj: mat4x4<f32>,
    cube_model: mat4x4<f32>,
    shadow_model: mat4x4<f32>,
    // x: premultiplied output flag, y: shadow strength, z: hover, w: unused
    params: vec4<f32>,
}

@group(0) @binding(0) var<uniform> u: Uniforms;

// ---------------------------------------------------------------------------
// Cube

struct CubeOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) normal: vec3<f32>,
    @location(1) color: vec3<f32>,
}

@vertex
fn vs_cube(
    @location(0) position: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) color: vec3<f32>,
) -> CubeOut {
    var out: CubeOut;
    out.pos = u.view_proj * (u.cube_model * vec4<f32>(position, 1.0));
    // Uniform scale: rotating the normal by the model rotation is enough.
    out.normal = (u.cube_model * vec4<f32>(normal, 0.0)).xyz;
    out.color = color;
    return out;
}

@fragment
fn fs_cube(in: CubeOut) -> @location(0) vec4<f32> {
    let n = normalize(in.normal);
    let l = normalize(vec3<f32>(0.45, 0.85, 0.6));
    let diffuse = max(dot(n, l), 0.0);
    var col = in.color * (0.42 + 0.68 * diffuse);

    // Hover: gentle warm brightening.
    let hover_tint = col * vec3<f32>(1.30, 1.18, 1.06) + vec3<f32>(0.04, 0.03, 0.01);
    col = mix(col, hover_tint, clamp(u.params.z, 0.0, 1.0));

    // Opaque cube: premultiplied and straight-alpha output are identical
    // at alpha = 1.
    return vec4<f32>(col, 1.0);
}

// ---------------------------------------------------------------------------
// Shadow (soft radial billboard quad, partially transparent by design —
// this is the artifact that proves partial alpha compositing)

struct ShadowOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
}

@vertex
fn vs_shadow(
    @location(0) position: vec3<f32>,
    @location(1) uv: vec2<f32>,
) -> ShadowOut {
    var out: ShadowOut;
    out.pos = u.view_proj * (u.shadow_model * vec4<f32>(position, 1.0));
    out.uv = uv;
    return out;
}

@fragment
fn fs_shadow(in: ShadowOut) -> @location(0) vec4<f32> {
    let d = distance(in.uv, vec2<f32>(0.5, 0.5)) * 2.0;
    let t = clamp((1.0 - d) / 0.62, 0.0, 1.0);
    // Manual smoothstep (valid for the decreasing direction used here).
    let falloff = t * t * (3.0 - 2.0 * t);
    let a = clamp(u.params.y, 0.0, 1.0) * falloff;
    if (a <= 0.001) {
        discard;
    }

    let base = vec3<f32>(0.03, 0.03, 0.06);
    if (u.params.x > 0.5) {
        // Premultiplied output: rgb already scaled by alpha.
        return vec4<f32>(base * a, a);
    } else {
        // Straight alpha output for PostMultiplied compositing.
        return vec4<f32>(base, a);
    }
}
