//! Bevy 0.20 rendering backend.
//!
//! Drives Bevy's render app against OpenCuteMate's native surface:
//! no `bevy_winit` (no second window, no engine-owned loop). The runtime's
//! scheduler remains the only frame driver — `app.update()` is called from
//! [`BevyRenderer::render`], exactly when the scheduler decides a frame
//! should happen. Design, sources and risk register:
//! `docs/bevy-integration.md`.

mod handles;
mod scene;

use bevy::app::{App, TaskPoolPlugin};
use bevy::asset::AssetPlugin;
use bevy::core_pipeline::CorePipelinePlugin;
use bevy::pbr::PbrPlugin;
use bevy::prelude::*;
use bevy::render::{
    settings::{RenderCreation, RenderResources},
    view::window::{ExtractedWindow, SurfaceData},
    RenderApp, RenderPlugin,
};
use bevy::transform::TransformPlugin;
use bevy::window::{Window, WindowResolution};
use ocm_character::model::CharacterModel;
use ocm_platform::NativeHandles;

use crate::scene::Camera as SceneCamera;
use crate::{FrameOutcome, RenderOpts, Renderer, RendererError, SceneState};
use ocm_character::ElementHitbox;

use scene::SceneEntities;

/// Create the wgpu instance/adapter/device/queue exactly like the wgpu
/// backend does (Vulkan only, no experimental features, no display handle
/// in the instance descriptor), packaged for Bevy's manual render creation.
fn create_render_resources(
    native: &handles::NativeSurface,
) -> Result<RenderResources, RendererError> {
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
        backends: wgpu::Backends::VULKAN,
        ..wgpu::InstanceDescriptor::new_without_display_handle()
    });

    // A temporary surface from our handles so the adapter is guaranteed
    // compatible with the layer-shell surface Bevy will create later.
    let surface = unsafe {
        instance.create_surface_unsafe(wgpu::SurfaceTargetUnsafe::RawHandle {
            raw_display_handle: Some(
                raw_window_handle::HasDisplayHandle::display_handle(native)
                    .unwrap()
                    .as_raw(),
            ),
            raw_window_handle: raw_window_handle::HasWindowHandle::window_handle(native)
                .unwrap()
                .as_raw(),
        })
    }
    .map_err(|e| RendererError::SurfaceCreation(e.to_string()))?;

    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::HighPerformance,
        compatible_surface: Some(&surface),
        force_fallback_adapter: false,
        apply_limit_buckets: false,
    }))
    .map_err(|e| RendererError::Adapter(e.to_string()))?;

    let adapter_info = adapter.get_info();
    tracing::info!(
        adapter = %adapter_info.name,
        backend = ?adapter_info.backend,
        "GPU adapter selected (manual render creation)"
    );

    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("ocm-bevy device"),
        required_features: wgpu::Features::empty(),
        required_limits: wgpu::Limits::default(),
        experimental_features: wgpu::ExperimentalFeatures::default(),
        memory_hints: wgpu::MemoryHints::default(),
        trace: wgpu::Trace::Off,
    }))
    .map_err(|e| RendererError::Device(e.to_string()))?;

    // The temporary surface must not outlive the native objects; Bevy
    // creates its own surface from the same handles inside the plugin.
    drop(surface);

    Ok(RenderResources(
        bevy::render::renderer::RenderDevice::from(device),
        bevy::render::renderer::RenderQueue::new(queue),
        bevy::render::renderer::RenderAdapterInfo::new(adapter_info),
        bevy::render::renderer::RenderAdapter::new(adapter),
        bevy::render::renderer::RenderInstance::new(instance),
    ))
}

/// The Bevy render backend. Owns the `App` (and with it every GPU
/// resource, the wgpu surface created from our handles, and the pipeline
/// cache). Dropping it before the platform session satisfies the same
/// lifetime contract as the wgpu backend (docs/architecture.md §6).
pub struct BevyRenderer {
    app: App,
    entities: SceneEntities,
    model: CharacterModel,
    /// Placement/hitbox camera, framed for the current logical size.
    camera: SceneCamera,
    logical: (u32, u32),
    /// Whether the first present already landed (see `warm_up`).
    warmed_up: bool,
}

impl BevyRenderer {
    /// Create the backend from platform-neutral native handles.
    ///
    /// # Lifetime contract
    /// Identical to the wgpu backend: the native display/window objects
    /// behind `handles` must outlive this renderer. The runtime enforces
    /// the teardown order (renderer before platform session).
    pub fn new(
        handles: &NativeHandles,
        logical: (u32, u32),
        buffer: (u32, u32),
        opts: &RenderOpts,
        model: &CharacterModel,
    ) -> Result<Self, RendererError> {
        let native = handles::NativeSurface::new(handles);

        let mut app = App::new();
        // Plugin order matters: RenderPlugin registers the Shader asset
        // type (and the core shader library) that CorePipelinePlugin and
        // PbrPlugin load during their own build — mirroring the ordering
        // of bevy_internal's DefaultPlugins (RenderPlugin first).
        app.add_plugins((
            TaskPoolPlugin::default(),
            bevy::diagnostic::FrameCountPlugin,
            bevy::time::TimePlugin,
            AssetPlugin::default(),
            // No default window: our layer-shell surface entity (spawned
            // below) is the only — and primary — window. The default
            // `primary_window: Some(Window::default())` would create a
            // phantom 1280×720 window that steals `PrimaryWindow` camera
            // resolution.
            bevy::window::WindowPlugin {
                primary_window: None,
                primary_cursor_options: None,
                ..default()
            },
            TransformPlugin,
            RenderPlugin {
                // Manual renderer creation with the same parameters as the
                // wgpu backend (Vulkan only, no experimental features,
                // adapter selected against our layer-shell surface). Bevy's
                // automatic path would pick an adapter without any
                // surface-compatibility check, because our window entity is
                // unknown to `RenderPlugin` at that point.
                render_creation: RenderCreation::Manual(Box::new(create_render_resources(
                    &native,
                )?)),
                // Compile pipelines inline during updates instead of in the
                // background: with an event-driven scheduler there is no
                // "later frame" to finish them in before we park.
                synchronous_pipeline_compilation: true,
                debug_flags: default(),
            },
            // Mirrors bevy_internal's DefaultPlugins sequence after
            // RenderPlugin: ImagePlugin registers Assets<Image> (needed by
            // CorePipeline's tonemapping at build time) and MeshPlugin the
            // mesh assets. LightPlugin stays in (PbrPlugin's shadow-map
            // extraction needs its resources) even though the PoC materials
            // are unlit.
            bevy::image::ImagePlugin::default(),
            bevy::mesh::MeshPlugin,
            bevy::light::LightPlugin,
            CorePipelinePlugin,
            PbrPlugin::default(),
        ));
        // Main-world camera plugin: registers ClearColor (extracted into
        // the render world each frame) and the visibility propagation
        // systems (without these, ViewVisibility stays hidden).
        app.add_plugins(bevy::camera::CameraPlugin);

        let window = scene::spawn_window(&mut app, &native, buffer, opts.vsync);
        let entities = scene::spawn_scene(&mut app, window, model);

        // Plugin finish + cleanup: the step a runner would do before the
        // first update (bevy_app's ScheduleRunnerPlugin does the same).
        app.finish();
        app.cleanup();

        Ok(Self {
            app,
            entities,
            model: model.clone(),
            camera: SceneCamera::new(logical),
            logical,
            warmed_up: false,
        })
    }

    /// Drive updates until the surface exists, the first present has
    /// landed and every queued pipeline has compiled. The first frames
    /// otherwise present with the fullscreen-blit pipeline still missing,
    /// which attaches an unwritten swapchain buffer (stale driver memory)
    /// that the compositor keeps showing after we park. Bounded; false if
    /// it never happened.
    fn warm_up(&mut self) -> bool {
        for _ in 0..16 {
            self.app.update();
            if self.first_present_landed() && self.pipeline_queue_empty() {
                return true;
            }
        }
        self.first_present_landed() && self.pipeline_queue_empty()
    }

    /// Whether the render world holds a configured surface whose window
    /// has already presented once.
    fn first_present_landed(&mut self) -> bool {
        let Some(render_app) = self.app.get_sub_app_mut(RenderApp) else {
            return false;
        };
        let world = render_app.world_mut();
        let mut query = world.query_filtered::<&ExtractedWindow, With<SurfaceData>>();
        query
            .iter(world)
            .any(|window| !window.needs_initial_present)
    }

    /// Whether the pipeline cache has finished compiling everything
    /// queued so far.
    fn pipeline_queue_empty(&mut self) -> bool {
        let Some(render_app) = self.app.get_sub_app_mut(RenderApp) else {
            return false;
        };
        let world = render_app.world();
        let cache = world.resource::<bevy::render::render_resource::PipelineCache>();
        cache.waiting_pipelines().next().is_none()
    }

    /// Log the negotiated surface configuration (the evidence line the
    /// regression suite greps for, matching the wgpu backend's log).
    fn log_surface_config(&mut self) {
        let Some(render_app) = self.app.get_sub_app_mut(RenderApp) else {
            return;
        };
        let world = render_app.world_mut();
        let mut query = world.query_filtered::<&ExtractedWindow, With<SurfaceData>>();
        if let Some(window) = query.iter(world).next() {
            let format = window
                .swap_chain_texture_format
                .map(|f| format!("{f:?}"))
                .unwrap_or_else(|| "unknown".into());
            tracing::info!(
                format = %format,
                alpha_mode = ?window.alpha_mode,
                present_mode = ?window.present_mode,
                "surface configured"
            );
        }
    }
}

impl Renderer for BevyRenderer {
    fn resize(&mut self, logical: (u32, u32), buffer: (u32, u32)) {
        let (w, h) = (buffer.0.max(1), buffer.1.max(1));
        {
            let world = self.app.world_mut();
            let Some(mut window) = world.get_mut::<Window>(self.entities.window) else {
                return;
            };
            if window.resolution.physical_width() == w
                && window.resolution.physical_height() == h
                && self.logical == logical
            {
                return;
            }
            window.resolution = WindowResolution::new(w, h);
        }
        if self.logical != logical {
            self.camera = SceneCamera::new(logical);
            self.logical = logical;
        }
        tracing::info!(?logical, ?buffer, "renderer resized");
    }

    fn render(&mut self, scene: &SceneState) -> Result<FrameOutcome, RendererError> {
        // 1. Mirror the scene snapshot into entities (the runtime has
        //    already armed the wl_surface.frame callback; see the ordering
        //    note in docs/architecture.md §8).
        scene::sync_scene(self.app.world_mut(), &self.entities, scene, &self.camera);

        // 2. One scheduler-driven frame: main schedules + Extract + Render
        //    + present, inline on this thread (non-pipelined rendering).
        self.app.update();

        // 3. First-frame warm-up: keep the initial frame going until the
        //    surface is created and has presented once, so the runtime's
        //    frame callback has a commit to latch onto and parking works.
        if !self.warmed_up {
            self.warmed_up = self.warm_up();
            if self.warmed_up {
                self.log_surface_config();
            } else {
                tracing::warn!("bevy surface did not present during warm-up");
            }
        }

        Ok(FrameOutcome {
            presented: true,
            skipped: None,
        })
    }

    fn character_hitboxes(&self, scene: &SceneState) -> Vec<ElementHitbox> {
        crate::scene::element_hitboxes(scene, &self.model, &self.camera)
    }
}
