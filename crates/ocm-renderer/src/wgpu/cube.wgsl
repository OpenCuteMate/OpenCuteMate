// OpenCuteMate character shader: box-part model + soft billboard shadow
// over a transparent background. Alpha-mode aware via u.params.
//
// One uniform layout serves both pipelines:
//   parts:   u.params.rgb = per-part tint multiplier (base × pose × hover)
//   shadow:  u.params.x = premultiplied-output flag, u.params.y = strength

struct SceneUniforms {
    view_proj: mat4x4<f32>,
    model: mat4x4<f32>,
    params: vec4<f32>,
}

@group(0) @binding(0) var<uniform> u: SceneUniforms;

// ---------------------------------------------------------------------------
// Character parts (opaque boxes)

struct PartOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) normal: vec3<f32>,
    @location(1) color: vec3<f32>,
}

@vertex
fn vs_part(
    @location(0) position: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) color: vec3<f32>,
) -> PartOut {
    var out: PartOut;
    out.pos = u.view_proj * (u.model * vec4<f32>(position, 1.0));
    // Squash is near-uniform; rotating the normal by the model rotation is
    // a good-enough shading approximation for this prototype.
    out.normal = (u.model * vec4<f32>(normal, 0.0)).xyz;
    out.color = color;
    return out;
}

@fragment
fn fs_part(in: PartOut) -> @location(0) vec4<f32> {
    let n = normalize(in.normal);
    let l = normalize(vec3<f32>(0.45, 0.85, 0.6));
    let diffuse = max(dot(n, l), 0.0);
    let col = in.color * u.params.rgb * (0.42 + 0.68 * diffuse);

    // Opaque parts: premultiplied and straight-alpha output are identical
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
    out.pos = u.view_proj * (u.model * vec4<f32>(position, 1.0));
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
