//! The PoC scene as Bevy entities, synchronized from [`SceneState`].
//!
//! Placement uses the exact same math as the wgpu backend
//! (`crate::math` + `crate::scene::Camera::placement`), so the character
//! renders at the same world position — and therefore the same logical
//! pixel position and hitbox — under both backends.

use bevy::app::App;
use bevy::prelude::*;
use bevy::window::PrimaryWindow;

use crate::math::{self, Mat4};
use crate::scene::Camera as SceneCamera;
use crate::SceneState;

use super::handles::NativeSurface;

/// Entity/material ids of the spawned scene, kept by the renderer.
pub(crate) struct SceneEntities {
    pub(crate) window: Entity,
    pub(crate) character: Entity,
    pub(crate) shadow: Entity,
    pub(crate) character_material: Handle<StandardMaterial>,
}

/// The window entity carrying our native handles; `bevy_render`'s
/// `WindowRenderPlugin` creates and reconfigures the wgpu surface from
/// exactly this component pair (see docs/bevy-integration.md §3).
pub(crate) fn spawn_window(
    app: &mut App,
    native: &NativeSurface,
    buffer: (u32, u32),
    vsync: bool,
) -> Entity {
    app.world_mut()
        .spawn((
            bevy::window::Window {
                resolution: bevy::window::WindowResolution::new(buffer.0.max(1), buffer.1.max(1)),
                present_mode: if vsync {
                    bevy::window::PresentMode::Fifo
                } else {
                    bevy::window::PresentMode::AutoNoVsync
                },
                // Wayland composites ARGB surfaces premultiplied; Bevy maps
                // this request onto the wgpu surface configuration verbatim.
                composite_alpha_mode: bevy::window::CompositeAlphaMode::PreMultiplied,
                ..default()
            },
            native.raw_handle_wrapper().expect("raw handle wrapper"),
            PrimaryWindow,
        ))
        .id()
}

/// Camera + character + shadow, framed by the shared scene constants.
/// `window` is the entity from `spawn_window` (kept for resize).
pub(crate) fn spawn_scene(app: &mut App, window: Entity) -> SceneEntities {
    let world = app.world_mut();

    // Camera: the constants from crate::scene (EYE/TARGET/FOV_Y/NEAR/FAR),
    // clearing to fully transparent so the compositor shows the desktop.
    // MSAA off: parity with the unantialiased wgpu reference renderer.
    // Tonemapping/dither off: parity with the wgpu renderer's raw color
    // output (no tone curve, no deband noise) — and no dependency on the
    // `tonemapping_luts` feature the default TonyMcMapface curve needs.
    // With the default tonemapper and no LUT feature, the tonemap node
    // errors, the final target is never written, and the window never
    // presents after the initial frames.
    world.spawn((
        Camera3d::default(),
        Projection::Perspective(PerspectiveProjection {
            fov: crate::scene::FOV_Y,
            near: crate::scene::NEAR,
            far: crate::scene::FAR,
            ..default()
        }),
        Camera {
            clear_color: ClearColorConfig::Custom(Color::NONE),
            ..default()
        },
        Msaa::Off,
        bevy::core_pipeline::tonemapping::Tonemapping::None,
        bevy::core_pipeline::tonemapping::DebandDither::Disabled,
        Transform::from_translation(crate::scene::EYE.into())
            .looking_at(crate::scene::TARGET.into(), Vec3::Y),
    ));

    // Meshes: unit cuboid spanning [-0.5, 0.5]³ (like the wgpu geometry)
    // and a [-1, 1]² billboard quad for the shadow.
    let mut meshes = world.resource_mut::<Assets<Mesh>>();
    let character_mesh = meshes.add(Mesh::from(Cuboid::new(1.0, 1.0, 1.0)));
    let shadow_mesh = meshes.add(Mesh::from(Cuboid::new(2.0, 2.0, 0.02)));

    let (character_material, shadow_material) = {
        let mut materials = world.resource_mut::<Assets<StandardMaterial>>();
        let character = materials.add(StandardMaterial {
            base_color: normal_color(), // coral, matching the wgpu cube's front face
            unlit: true,
            ..default()
        });
        let shadow = materials.add(StandardMaterial {
            base_color: Color::WHITE,
            // Straight alpha; blending over the (0,0,0,0) clear leaves
            // premultiplied-correct values in the buffer for the compositor.
            alpha_mode: AlphaMode::Blend,
            unlit: true,
            ..default()
        });
        (character, shadow)
    };
    // Soft radial shadow: a procedural alpha texture reproducing the wgpu
    // shader's falloff (cube.wgsl fs_shadow), so the Bevy shadow band has
    // the same partial-alpha footprint as the reference renderer.
    let shadow_texture = world
        .resource_mut::<Assets<Image>>()
        .add(radial_shadow_texture());
    world
        .resource_mut::<Assets<StandardMaterial>>()
        .get_mut(&shadow_material)
        .expect("shadow material")
        .base_color_texture = Some(shadow_texture);

    let character = world
        .spawn((
            Mesh3d(character_mesh),
            MeshMaterial3d(character_material.clone()),
            Transform::IDENTITY,
        ))
        .id();
    let shadow = world
        .spawn((
            Mesh3d(shadow_mesh),
            MeshMaterial3d(shadow_material),
            Transform::IDENTITY,
        ))
        .id();

    SceneEntities {
        window,
        character,
        shadow,
        character_material,
    }
}

/// Character base color while hovered: a gentle warm brightening, the
/// Bevy-path equivalent of the wgpu shader's hover tint.
fn hover_color() -> Color {
    Color::srgb(1.0, 0.55, 0.42)
}

fn normal_color() -> Color {
    Color::srgb(0.94, 0.38, 0.28)
}

/// A radial-falloff alpha texture for the shadow quad, reproducing
/// `wgpu/cube.wgsl`'s `fs_shadow` curve: `d = dist(uv, center) * 2`,
/// `t = clamp((1-d)/0.62, 0, 1)`, smoothstepped, times strength 0.42 —
/// with the same near-black warm base color (authored in sRGB, matching
/// the linear values the wgpu shader outputs).
fn radial_shadow_texture() -> Image {
    const SIZE: u32 = 256;
    // srgb(0.03, 0.03, 0.06) — the shadow base color from the wgpu shader.
    let (base_r, base_g, base_b) = (0.191 * 255.0, 0.191 * 255.0, 0.262 * 255.0);
    let mut data = Vec::with_capacity((SIZE * SIZE * 4) as usize);
    for y in 0..SIZE {
        for x in 0..SIZE {
            let uv = (
                (x as f32 + 0.5) / SIZE as f32,
                (y as f32 + 0.5) / SIZE as f32,
            );
            let d = ((uv.0 - 0.5).powi(2) + (uv.1 - 0.5).powi(2)).sqrt() * 2.0;
            let t = ((1.0 - d) / 0.62).clamp(0.0, 1.0);
            let falloff = t * t * (3.0 - 2.0 * t);
            let a = (0.42 * falloff).clamp(0.0, 1.0);
            data.extend_from_slice(&[base_r as u8, base_g as u8, base_b as u8, (a * 255.0) as u8]);
        }
    }
    Image::new(
        bevy::render::render_resource::Extent3d {
            width: SIZE,
            height: SIZE,
            depth_or_array_layers: 1,
        },
        bevy::render::render_resource::TextureDimension::D2,
        data,
        bevy::render::render_resource::TextureFormat::Rgba8UnormSrgb,
        bevy::asset::RenderAssetUsages::MAIN_WORLD | bevy::asset::RenderAssetUsages::RENDER_WORLD,
    )
}

/// Write one `SceneState` snapshot into the entities before a frame is
/// extracted. The model matrices are computed by the same formulas (and
/// the same `crate::math`) as `crate::wgpu`'s renderer; `camera` is the
/// shared placement camera the renderer keeps framed for the current
/// logical size.
pub(crate) fn sync_scene(
    world: &mut World,
    entities: &SceneEntities,
    state: &SceneState,
    camera: &SceneCamera,
) {
    // Hover tint first: swap the material's base color (cheap; materials
    // are assets and change detection drives re-extraction).
    let mut materials = world.resource_mut::<Assets<StandardMaterial>>();
    if let Some(mut mat) = materials.get_mut(&entities.character_material) {
        mat.base_color = if state.hover {
            hover_color()
        } else {
            normal_color()
        };
    }

    let placement = frame_placement(state, camera);

    let mut character = world.entity_mut(entities.character);
    character.insert(Transform::from_matrix(to_glam(placement.cube_model)));

    let mut shadow = world.entity_mut(entities.shadow);
    shadow.insert(Transform::from_matrix(to_glam(placement.shadow_model)));
}

/// The per-frame model matrices (a pure function of the scene state, shared
/// with the wgpu backend's render math).
struct FramePlacement {
    cube_model: Mat4,
    shadow_model: Mat4,
}

fn frame_placement(state: &SceneState, camera: &SceneCamera) -> FramePlacement {
    let placement = camera.placement(state);

    let bob_h = if state.animate {
        placement.world_h * 0.055 * (state.time * 2.2).sin()
    } else {
        0.0
    };
    let yaw = if state.animate { state.time * 0.8 } else { 0.0 };

    let cube_model = math::mat_mul(
        &math::translate(
            placement.center[0],
            placement.center[1] + bob_h * placement.up[1],
            placement.center[2],
        ),
        &math::mat_mul(&math::rotate_y(yaw), &math::scale(placement.world_h)),
    );
    let shadow_model = math::billboard(
        [
            placement.center[0] - placement.up[0] * placement.world_h * 0.72,
            placement.center[1] - placement.up[1] * placement.world_h * 0.72 - bob_h * 0.5,
            placement.center[2] - placement.up[2] * placement.world_h * 0.72,
        ],
        [
            placement.right[0] * placement.world_h * 0.95,
            placement.right[1] * placement.world_h * 0.95,
            placement.right[2] * placement.world_h * 0.95,
        ],
        [
            placement.up[0] * placement.world_h * 0.30,
            placement.up[1] * placement.world_h * 0.30,
            placement.up[2] * placement.world_h * 0.30,
        ],
        placement.forward,
    );

    FramePlacement {
        cube_model,
        shadow_model,
    }
}

/// Column-major `[[f32; 4]; 4]` (m[col][row], the WGSL layout our math uses)
/// into a glam matrix, whose columns come first as well.
fn to_glam(m: Mat4) -> bevy::math::Mat4 {
    bevy::math::Mat4::from_cols_array_2d(&m)
}
