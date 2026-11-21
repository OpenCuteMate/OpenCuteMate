//! The character model as Bevy entities, synchronized from
//! [`SceneState`].
//!
//! Placement uses the exact same math as the wgpu backend
//! (`crate::math` + `crate::scene`), so the character renders at the same
//! world position — and therefore the same logical pixel position and
//! hitbox — under both backends.
//!
//! Entity shape mirrors the model's element hierarchy: a root entity
//! (per-frame composed root pose) with one child entity per element and
//! one grandchild per visual part. Element poses (sleep-flattened eyes)
//! scale the element entities; hover tints the part materials. A future
//! skeleton path maps bones onto the same hierarchy.

use bevy::app::App;
use bevy::prelude::*;
use bevy::window::PrimaryWindow;

use crate::math::Mat4;
use crate::scene::Camera as SceneCamera;
use crate::SceneState;

use super::handles::NativeSurface;

/// Entity/material handles of the spawned scene, kept by the renderer.
pub(crate) struct SceneEntities {
    pub(crate) window: Entity,
    pub(crate) root: Entity,
    pub(crate) shadow: Entity,
    /// (element entity, element id) pairs.
    pub(crate) elements: Vec<(Entity, ocm_character::ElementId)>,
    /// (part material, its element id, base sRGB color). Part entities
    /// are static (local transform never changes — element poses scale
    /// the element entities), so only the materials are updated per frame.
    pub(crate) parts: Vec<(Handle<StandardMaterial>, ocm_character::ElementId, [f32; 3])>,
    /// The shadow material (strength follows the bob each frame).
    pub(crate) shadow_material: Handle<StandardMaterial>,
}

/// A part to spawn: local placement plus its material and base color.
struct PartSpec {
    offset: [f32; 3],
    size: [f32; 3],
    material: Handle<StandardMaterial>,
    base_color: [f32; 3],
}

/// The window entity carrying our native handles; `bevy_render`'s
/// `WindowRenderPlugin` creates and reconfigures the wgpu surface from
/// exactly this component pair (see docs/bevy-integration.md §3).
pub(crate) fn spawn_window(
    app: &mut App,
    native: &NativeSurface,
    buffer: (u32, u32),
    vsync: bool,
    composite_alpha_mode: bevy::window::CompositeAlphaMode,
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
                composite_alpha_mode,
                ..default()
            },
            native.raw_handle_wrapper().expect("raw handle wrapper"),
            PrimaryWindow,
        ))
        .id()
}

/// Camera + character part hierarchy + shadow, framed by the shared scene
/// constants. `window` is the entity from `spawn_window` (kept for
/// resize).
pub(crate) fn spawn_scene(
    app: &mut App,
    window: Entity,
    model: &ocm_character::CharacterModel,
) -> SceneEntities {
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

    // Meshes: one unit cuboid per visual part (the per-frame transform
    // does the placement/scale) and a [-1, 1]² billboard quad for the
    // shadow.
    let mut meshes = world.resource_mut::<Assets<Mesh>>();
    let part_mesh = meshes.add(Mesh::from(Cuboid::new(1.0, 1.0, 1.0)));
    let shadow_mesh = meshes.add(Mesh::from(Cuboid::new(2.0, 2.0, 0.02)));

    // Root entity: the composed root transform is written per frame.
    let root = world.spawn(Transform::IDENTITY).id();

    // Create every part material first (assets borrow the world), then
    // spawn the hierarchy.
    let mut elements = Vec::new();
    let mut parts = Vec::new();
    let spec_data: Vec<(ocm_character::ElementId, [f32; 3], Vec<PartSpec>)> = {
        let mut materials = world.resource_mut::<Assets<StandardMaterial>>();
        model
            .elements()
            .iter()
            .map(|element| {
                let parts: Vec<PartSpec> = element
                    .parts
                    .iter()
                    .map(|part| {
                        let base_color = [
                            part.color[0].clamp(0.0, 1.0),
                            part.color[1].clamp(0.0, 1.0),
                            part.color[2].clamp(0.0, 1.0),
                        ];
                        let material = materials.add(StandardMaterial {
                            base_color: Color::srgb(base_color[0], base_color[1], base_color[2]),
                            unlit: true,
                            ..default()
                        });
                        PartSpec {
                            offset: part.offset,
                            size: part.size,
                            material,
                            base_color,
                        }
                    })
                    .collect();
                (element.id, element.offset, parts)
            })
            .collect()
    };
    for (element_id, element_offset, part_specs) in spec_data {
        let element_entity = world
            .spawn((
                Transform::from_translation(element_offset.into()),
                ChildOf(root),
            ))
            .id();
        elements.push((element_entity, element_id));
        for spec in part_specs {
            let _part_entity = world
                .spawn((
                    Mesh3d(part_mesh.clone()),
                    MeshMaterial3d(spec.material.clone()),
                    Transform::from_translation(spec.offset.into()).with_scale(Vec3::new(
                        spec.size[0],
                        spec.size[1],
                        spec.size[2],
                    )),
                    ChildOf(element_entity),
                ))
                .id();
            parts.push((spec.material, element_id, spec.base_color));
        }
    }

    // Soft radial shadow: a procedural alpha texture reproducing the wgpu
    // shader's falloff (cube.wgsl fs_shadow), so the Bevy shadow band has
    // the same partial-alpha footprint as the reference renderer.
    let shadow_material = world
        .resource_mut::<Assets<StandardMaterial>>()
        .add(StandardMaterial {
            base_color: Color::WHITE,
            // Straight alpha; blending over the (0,0,0,0) clear leaves
            // premultiplied-correct values in the buffer for the compositor.
            alpha_mode: AlphaMode::Blend,
            unlit: true,
            ..default()
        });
    let shadow_texture = world
        .resource_mut::<Assets<Image>>()
        .add(radial_shadow_texture());
    world
        .resource_mut::<Assets<StandardMaterial>>()
        .get_mut(&shadow_material)
        .expect("shadow material")
        .base_color_texture = Some(shadow_texture);
    let shadow = world
        .spawn((
            Mesh3d(shadow_mesh),
            MeshMaterial3d(shadow_material.clone()),
            Transform::IDENTITY,
        ))
        .id();

    SceneEntities {
        window,
        root,
        shadow,
        elements,
        parts,
        shadow_material,
    }
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
/// extracted. All matrices come from the shared placement math
/// (`crate::scene`), so both backends stay pixel-identical.
pub(crate) fn sync_scene(
    world: &mut World,
    entities: &SceneEntities,
    state: &SceneState,
    camera: &SceneCamera,
) {
    let placement = camera.placement(state);

    // Root pose + shadow from the shared composition functions.
    let root_model: Mat4 = crate::scene::root_model(&placement, &state.pose.root);
    let shadow_model: Mat4 = crate::scene::shadow_model(&placement, &state.pose.root);

    world
        .entity_mut(entities.root)
        .insert(Transform::from_matrix(to_glam(root_model)));
    world
        .entity_mut(entities.shadow)
        .insert(Transform::from_matrix(to_glam(shadow_model)));

    // Element poses: per-element scale (sleeping flattens the eyes).
    for (entity, id) in &entities.elements {
        let scale = state.pose.element(*id).scale;
        let Ok(mut element) = world.get_entity_mut(*entity) else {
            continue;
        };
        let mut transform = element.get_mut::<Transform>().expect("element transform");
        transform.scale = Vec3::new(scale[0].max(0.01), scale[1].max(0.01), scale[2].max(0.01));
    }

    // Part tints: base × pose tint × hover.
    {
        let mut materials = world.resource_mut::<Assets<StandardMaterial>>();
        for (material, element_id, base_color) in &entities.parts {
            let mut tint = state.pose.element(*element_id).tint;
            if state.hover == Some(*element_id) {
                tint = [
                    (tint[0] * 1.22 + 0.02).min(1.0),
                    (tint[1] * 1.16 + 0.015).min(1.0),
                    (tint[2] * 1.06 + 0.01).min(1.0),
                ];
            }
            let target = Color::srgb(
                (base_color[0] * tint[0]).min(1.0),
                (base_color[1] * tint[1]).min(1.0),
                (base_color[2] * tint[2]).min(1.0),
            );
            let Some(mut mat) = materials.get_mut(material) else {
                continue;
            };
            // Assign only on change so unchanged frames do not dirty the
            // asset (re-extraction costs a frame).
            if mat.base_color != target {
                mat.base_color = target;
            }
        }
    }

    // Shadow strength follows the bob (shared formula).
    let strength = crate::scene::shadow_strength(&state.pose.root);
    {
        let mut materials = world.resource_mut::<Assets<StandardMaterial>>();
        let Some(mut mat) = materials.get_mut(&entities.shadow_material) else {
            return;
        };
        // The texture already carries the 0.42 falloff; the material
        // scales it by strength/0.42.
        let alpha = (strength / 0.42).clamp(0.0, 1.0);
        let target = Color::srgba(1.0, 1.0, 1.0, alpha);
        if mat.base_color != target {
            mat.base_color = target;
        }
    }
}

/// Column-major `[[f32; 4]; 4]` (m[col][row], the WGSL layout our math uses)
/// into a glam matrix, whose columns come first as well.
fn to_glam(m: Mat4) -> bevy::math::Mat4 {
    bevy::math::Mat4::from_cols_array_2d(&m)
}
