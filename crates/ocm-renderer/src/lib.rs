//! # ocm-renderer
//!
//! wgpu-direct renderer for the OpenCuteMate PoC: a procedural cube with a
//! soft drop shadow over a fully transparent background, driven by
//! [`SceneState`] snapshots.
//!
//! This crate never touches a Wayland type: it consumes
//! [`ocm_platform::NativeHandles`] (platform-neutral raw-window-handle
//! enums) and sizes. The only `unsafe` is the documented
//! `create_surface_unsafe` call (see below).

mod math;
mod scene;

use ocm_platform::NativeHandles;
use scene::Camera;
use wgpu::util::DeviceExt;

/// Renderer construction options.
#[derive(Debug, Clone, Copy)]
pub struct RenderOpts {
    /// `true` = Fifo (vsync), `false` = best-effort no-vsync.
    pub vsync: bool,
}

/// A snapshot of the character state to render.
#[derive(Debug, Clone, Copy)]
pub struct SceneState {
    /// Normalized position [0,1] x [0,1], top-left origin.
    pub position: (f64, f64),
    /// Character height as a fraction of the logical output height.
    pub size_frac: f32,
    /// Pointer hover highlight.
    pub hover: bool,
    /// Continuous animation (bob + slow spin).
    pub animate: bool,
    /// Seconds since start (drives animation).
    pub time: f32,
}

/// Hitbox of the character in logical surface pixels.
#[derive(Debug, Clone, Copy)]
pub struct Hitbox {
    pub center_x: f32,
    pub center_y: f32,
    pub radius: f32,
}

impl Hitbox {
    pub fn contains(&self, x: f64, y: f64) -> bool {
        let dx = x as f32 - self.center_x;
        let dy = y as f32 - self.center_y;
        dx * dx + dy * dy <= self.radius * self.radius
    }
}

/// Result of one `render` call.
#[derive(Debug, Clone, Copy)]
pub struct FrameOutcome {
    /// Whether a frame was actually acquired, encoded and presented.
    pub presented: bool,
    /// Reason the frame was skipped (the scheduler should retry later).
    pub skipped: Option<&'static str>,
}

/// Negotiated surface configuration (for logging/evidence).
#[derive(Debug, Clone)]
pub struct SurfaceInfo {
    pub adapter: String,
    pub backend: &'static str,
    pub format: wgpu::TextureFormat,
    pub alpha_mode: wgpu::CompositeAlphaMode,
    pub present_mode: wgpu::PresentMode,
}

#[derive(Debug, thiserror::Error)]
pub enum RendererError {
    #[error("failed to create the wgpu surface: {0}")]
    SurfaceCreation(String),
    #[error("no GPU adapter compatible with the surface was found: {0}")]
    Adapter(String),
    #[error("failed to acquire a GPU device: {0}")]
    Device(String),
    #[error("no surface format with an alpha channel is supported (transparency impossible): available = {0:?}")]
    NoAlphaFormat(Vec<wgpu::TextureFormat>),
    #[error("no alpha-compositing mode that supports transparency is available: {0:?}")]
    NoAlphaMode(Vec<wgpu::CompositeAlphaMode>),
    #[error("the GPU surface was lost: {0}")]
    SurfaceLost(String),
    #[error("render failed: {0}")]
    Render(String),
}

/// Uniform buffer layout, shared with `cube.wgsl` (WGSL uniform layout:
/// mat4 align 16, vec4 align 16 — matches this repr(C) struct).
#[repr(C)]
#[derive(Debug, Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Uniforms {
    view_proj: [[f32; 4]; 4],
    cube_model: [[f32; 4]; 4],
    shadow_model: [[f32; 4]; 4],
    /// x: premultiplied-output flag, y: shadow strength, z: hover, w: unused
    params: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct CubeVertex {
    position: [f32; 3],
    normal: [f32; 3],
    color: [f32; 3],
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct ShadowVertex {
    position: [f32; 3],
    uv: [f32; 2],
    _pad: [f32; 3], // keep stride friendly (24 → 32 bytes)
}

/// The wgpu renderer.
pub struct WgpuRenderer {
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    premultiplied: bool,
    uniform_buffer: wgpu::Buffer,
    bind_group: wgpu::BindGroup,
    cube_pipeline: wgpu::RenderPipeline,
    shadow_pipeline: wgpu::RenderPipeline,
    cube_vb: wgpu::Buffer,
    cube_ib: wgpu::Buffer,
    cube_index_count: u32,
    shadow_vb: wgpu::Buffer,
    shadow_ib: wgpu::Buffer,
    shadow_index_count: u32,
    camera: Camera,
    /// Logical surface size the camera is framed for.
    logical: (u32, u32),
    info: SurfaceInfo,
}

impl WgpuRenderer {
    /// Create the renderer from platform-neutral native handles.
    ///
    /// # Lifetime contract
    /// The native display/window objects behind `handles` must outlive this
    /// renderer (wgpu requires the raw handles stay valid until the returned
    /// `Surface` is dropped). The runtime enforces this by dropping the
    /// renderer before the platform session (see docs/architecture.md §6).
    pub fn new(
        handles: &NativeHandles,
        logical: (u32, u32),
        buffer: (u32, u32),
        opts: &RenderOpts,
    ) -> Result<Self, RendererError> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::VULKAN,
            ..wgpu::InstanceDescriptor::new_without_display_handle()
        });

        // SAFETY: create_surface_unsafe requires that the display and window
        // handles are valid for the entire lifetime of the returned Surface.
        //
        // Why unsafe is required: wgpu has no safe constructor for a GPU
        // surface owned by an external (non-winit) windowing stack; the raw
        // pointers cannot be lifetime-checked across the FFI boundary.
        //
        // Why this is sound here:
        // 1. `handles` was produced from *live* wayland objects (`wl_display`
        //    via Connection::backend().display_ptr(), `wl_surface` via
        //    ObjectId::as_ptr()) that are stored in the platform's shared
        //    state.
        // 2. The runtime's teardown order drops this renderer (and therefore
        //    the wgpu Surface) strictly before the platform session drops
        //    those objects (explicit `drop(app)` before `drop(event_loop)` in
        //    ocm-runtime's main()).
        // 3. All handle use is single-threaded; no aliasing of the pointers
        //    occurs outside the graphics stack.
        let surface = unsafe {
            instance.create_surface_unsafe(wgpu::SurfaceTargetUnsafe::RawHandle {
                raw_display_handle: Some(handles.display),
                raw_window_handle: handles.window,
            })
        }
        .map_err(|e| RendererError::SurfaceCreation(e.to_string()))?;

        let adapter = pollster::block_on(instance.request_adapter(
            &wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                compatible_surface: Some(&surface),
                force_fallback_adapter: false,
                apply_limit_buckets: false,
            },
        ))
        .map_err(|e| RendererError::Adapter(e.to_string()))?;

        let adapter_info = adapter.get_info();
        tracing::info!(
            adapter = %adapter_info.name,
            backend = ?adapter_info.backend,
            driver = ?adapter_info.driver,
            "GPU adapter selected"
        );

        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("ocm-renderer device"),
            required_features: wgpu::Features::empty(),
            required_limits: wgpu::Limits::default(),
            experimental_features: wgpu::ExperimentalFeatures::default(),
            memory_hints: wgpu::MemoryHints::default(),
            trace: wgpu::Trace::Off,
        }))
        .map_err(|e| RendererError::Device(e.to_string()))?;

        let caps = surface.get_capabilities(&adapter);
        tracing::info!(
            formats = ?caps.formats,
            alpha_modes = ?caps.alpha_modes,
            present_modes = ?caps.present_modes,
            usages = ?caps.usages,
            "surface capabilities"
        );

        // Format: prefer 8-bit UNORM alpha formats (the classic ARGB8888
        // family compositors expect, and a direct match for hand-authored
        // sRGB-ish shader colors), then any other non-sRGB alpha format,
        // then any alpha format at all.
        let format = caps
            .formats
            .iter()
            .copied()
            .find(|f| {
                !f.is_srgb()
                    && matches!(
                        f,
                        wgpu::TextureFormat::Bgra8Unorm | wgpu::TextureFormat::Rgba8Unorm
                    )
            })
            .or_else(|| {
                caps.formats
                    .iter()
                    .copied()
                    .find(|f| format_has_alpha(*f) && !f.is_srgb())
            })
            .or_else(|| caps.formats.iter().copied().find(|f| format_has_alpha(*f)))
            .ok_or_else(|| RendererError::NoAlphaFormat(caps.formats.clone()))?;

        // Alpha mode: never hardcoded — queried and picked by priority.
        // PreMultiplied and Inherit both mean premultiplied compositing on
        // Wayland (Inherit = platform default = premultiplied ARGB).
        let alpha_mode = caps
            .alpha_modes
            .iter()
            .copied()
            .find(|m| *m == wgpu::CompositeAlphaMode::PreMultiplied)
            .or_else(|| {
                caps.alpha_modes
                    .iter()
                    .copied()
                    .find(|m| *m == wgpu::CompositeAlphaMode::Inherit)
            })
            .or_else(|| {
                caps.alpha_modes
                    .iter()
                    .copied()
                    .find(|m| *m == wgpu::CompositeAlphaMode::PostMultiplied)
            })
            .ok_or_else(|| RendererError::NoAlphaMode(caps.alpha_modes.clone()))?;
        let premultiplied = !matches!(alpha_mode, wgpu::CompositeAlphaMode::PostMultiplied);
        if !premultiplied {
            tracing::warn!("compositor requires PostMultiplied alpha; shader emits straight alpha");
        }

        // Present mode: Fifo for vsync; AutoNoVsync falls back gracefully.
        let present_mode = if opts.vsync {
            wgpu::PresentMode::Fifo
        } else {
            wgpu::PresentMode::AutoNoVsync
        };

        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            color_space: wgpu::SurfaceColorSpace::Auto,
            width: buffer.0.max(1),
            height: buffer.1.max(1),
            present_mode,
            desired_maximum_frame_latency: 2,
            alpha_mode,
            view_formats: Vec::new(),
        };
        surface.configure(&device, &config);

        let info = SurfaceInfo {
            adapter: format!("{} ({:?})", adapter_info.name, adapter_info.backend),
            backend: "Vulkan",
            format,
            alpha_mode,
            present_mode,
        };
        tracing::info!(?format, ?alpha_mode, ?present_mode, "surface configured");

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("cube shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("cube.wgsl").into()),
        });

        // Uniform bind group.
        let uniform_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("scene uniforms"),
            size: std::mem::size_of::<Uniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("scene bind group layout"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: wgpu::BufferSize::new(
                        std::mem::size_of::<Uniforms>() as u64,
                    ),
                },
                count: None,
            }],
        });
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("scene bind group"),
            layout: &bgl,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: uniform_buffer.as_entire_binding(),
            }],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("scene pipeline layout"),
            bind_group_layouts: &[Some(&bgl)],
            immediate_size: 0,
        });

        let cube_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("cube pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_cube"),
                buffers: &[Some(wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<CubeVertex>() as u64,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &[
                        wgpu::VertexAttribute {
                            shader_location: 0,
                            format: wgpu::VertexFormat::Float32x3,
                            offset: 0,
                        },
                        wgpu::VertexAttribute {
                            shader_location: 1,
                            format: wgpu::VertexFormat::Float32x3,
                            offset: 12,
                        },
                        wgpu::VertexAttribute {
                            shader_location: 2,
                            format: wgpu::VertexFormat::Float32x3,
                            offset: 24,
                        },
                    ],
                })],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_cube"),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: None, // single-pass composition; alpha handled in shader
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                cull_mode: Some(wgpu::Face::Back),
                ..wgpu::PrimitiveState::default()
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });

        let shadow_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("shadow pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_shadow"),
                buffers: &[Some(wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<ShadowVertex>() as u64,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &[
                        wgpu::VertexAttribute {
                            shader_location: 0,
                            format: wgpu::VertexFormat::Float32x3,
                            offset: 0,
                        },
                        wgpu::VertexAttribute {
                            shader_location: 1,
                            format: wgpu::VertexFormat::Float32x2,
                            offset: 12,
                        },
                    ],
                })],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_shadow"),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                cull_mode: None, // billboard quad
                ..wgpu::PrimitiveState::default()
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });

        // Geometry: procedural cube (24 verts) + shadow quad.
        let (cube_vertices, cube_indices) = cube_geometry();
        let cube_vb = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("cube vertex buffer"),
            contents: bytemuck::cast_slice(&cube_vertices),
            usage: wgpu::BufferUsages::VERTEX,
        });
        let cube_ib = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("cube index buffer"),
            contents: bytemuck::cast_slice(&cube_indices),
            usage: wgpu::BufferUsages::INDEX,
        });
        let shadow_vertices = [
            ShadowVertex { position: [-1.0, -1.0, 0.0], uv: [0.0, 0.0], _pad: [0.0; 3] },
            ShadowVertex { position: [1.0, -1.0, 0.0], uv: [1.0, 0.0], _pad: [0.0; 3] },
            ShadowVertex { position: [1.0, 1.0, 0.0], uv: [1.0, 1.0], _pad: [0.0; 3] },
            ShadowVertex { position: [-1.0, 1.0, 0.0], uv: [0.0, 1.0], _pad: [0.0; 3] },
        ];
        let shadow_indices: [u16; 6] = [0, 1, 2, 0, 2, 3];
        let shadow_vb = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("shadow vertex buffer"),
            contents: bytemuck::cast_slice(&shadow_vertices),
            usage: wgpu::BufferUsages::VERTEX,
        });
        let shadow_ib = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("shadow index buffer"),
            contents: bytemuck::cast_slice(&shadow_indices),
            usage: wgpu::BufferUsages::INDEX,
        });
        let shadow_index_count = shadow_indices.len() as u32;

        let camera = Camera::new(logical);

        Ok(Self {
            surface,
            device,
            queue,
            config,
            premultiplied,
            uniform_buffer,
            bind_group,
            cube_pipeline,
            shadow_pipeline,
            cube_vb,
            cube_ib,
            cube_index_count: cube_indices.len() as u32,
            shadow_vb,
            shadow_ib,
            shadow_index_count,
            camera,
            logical,
            info,
        })
    }

    /// Resize to new logical/buffer dimensions (reconfigures the surface).
    pub fn resize(&mut self, logical: (u32, u32), buffer: (u32, u32)) {
        if self.config.width == buffer.0.max(1)
            && self.config.height == buffer.1.max(1)
            && self.logical == logical
        {
            return;
        }
        self.config.width = buffer.0.max(1);
        self.config.height = buffer.1.max(1);
        self.surface.configure(&self.device, &self.config);
        self.camera = Camera::new(logical);
        self.logical = logical;
        tracing::info!(?logical, ?buffer, "renderer resized");
    }

    /// The negotiated surface configuration (evidence for the test suite).
    pub fn info(&self) -> &SurfaceInfo {
        &self.info
    }

    /// Character hitbox in logical surface pixels for a given scene.
    /// Uses the same placement math as `render`, so input regions always
    /// match what is drawn.
    pub fn character_hitbox(&self, scene: &SceneState) -> Hitbox {
        let placement = self.camera.placement(scene, self.logical);
        Hitbox {
            center_x: placement.hitbox_center_px.0,
            center_y: placement.hitbox_center_px.1,
            radius: placement.hitbox_radius_px,
        }
    }

    /// Render one frame from a scene snapshot.
    pub fn render(&mut self, scene: &SceneState) -> Result<FrameOutcome, RendererError> {
        let frame = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(tex) => tex,
            wgpu::CurrentSurfaceTexture::Suboptimal(tex) => {
                tracing::debug!("surface texture suboptimal; continuing");
                tex
            }
            wgpu::CurrentSurfaceTexture::Timeout => {
                return Ok(FrameOutcome { presented: false, skipped: Some("timeout") });
            }
            wgpu::CurrentSurfaceTexture::Occluded => {
                return Ok(FrameOutcome { presented: false, skipped: Some("occluded") });
            }
            wgpu::CurrentSurfaceTexture::Outdated => {
                // Size state is driven by layer-shell configure events; the
                // next Configure/ScaleChanged will reconfigure. Retry soon.
                tracing::debug!("surface outdated; reconfiguring with last known size");
                self.surface.configure(&self.device, &self.config);
                return Ok(FrameOutcome { presented: false, skipped: Some("outdated") });
            }
            wgpu::CurrentSurfaceTexture::Lost => {
                return Err(RendererError::SurfaceLost("surface lost".into()));
            }
            wgpu::CurrentSurfaceTexture::Validation => {
                return Err(RendererError::Render("surface validation error".into()));
            }
        };

        let placement = self.camera.placement(scene, self.logical);
        let bob_h = if scene.animate {
            placement.world_h * 0.055 * (scene.time * 2.2).sin()
        } else {
            0.0
        };
        let yaw = if scene.animate { scene.time * 0.8 } else { 0.0 };

        // Lift the cube by the bob along the camera up axis.
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

        let shadow_strength = 0.42 * (1.0 - (bob_h / (placement.world_h * 0.06)).abs().clamp(0.0, 1.0) * 0.45);
        let uniforms = Uniforms {
            view_proj: placement.view_proj,
            cube_model,
            shadow_model,
            params: [
                if self.premultiplied { 1.0 } else { 0.0 },
                shadow_strength,
                if scene.hover { 1.0 } else { 0.0 },
                0.0,
            ],
        };
        self.queue.write_buffer(
            &self.uniform_buffer,
            0,
            bytemuck::bytes_of(&uniforms),
        );

        let view = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("ocm frame encoder"),
            });

        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("ocm render pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        // Fully transparent clear: (0,0,0,0) is both a
                        // valid premultiplied and straight-alpha "empty".
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: 0.0,
                            g: 0.0,
                            b: 0.0,
                            a: 0.0,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });

            // Painter's order: shadow first, cube over it. No blending —
            // each fragment computes its final alpha-mode-correct color.
            pass.set_bind_group(0, &self.bind_group, &[]);
            pass.set_pipeline(&self.shadow_pipeline);
            pass.set_vertex_buffer(0, self.shadow_vb.slice(..));
            pass.set_index_buffer(self.shadow_ib.slice(..), wgpu::IndexFormat::Uint16);
            pass.draw_indexed(0..self.shadow_index_count, 0, 0..1);

            pass.set_pipeline(&self.cube_pipeline);
            pass.set_vertex_buffer(0, self.cube_vb.slice(..));
            pass.set_index_buffer(self.cube_ib.slice(..), wgpu::IndexFormat::Uint16);
            pass.draw_indexed(0..self.cube_index_count, 0, 0..1);
        }

        self.queue.submit(std::iter::once(encoder.finish()));
        // Queue::present attaches the swapchain image (dmabuf) to the
        // wl_surface and commits it — the commit that also applies any
        // pending input region / viewport state set before this render.
        // (wgpu 30 moved present from SurfaceTexture to Queue.)
        self.queue.present(frame);

        Ok(FrameOutcome { presented: true, skipped: None })
    }
}

/// Whether a surface format carries per-pixel alpha. All realistic wgpu
/// surface formats do; this guard makes a transparency-impossible
/// environment fail loudly instead of rendering an opaque box.
fn format_has_alpha(format: wgpu::TextureFormat) -> bool {
    use wgpu::TextureFormat::*;
    matches!(
        format,
        Rgba8Unorm
            | Rgba8UnormSrgb
            | Bgra8Unorm
            | Bgra8UnormSrgb
            | Rgba16Float
            | Rgb10a2Unorm
            | Rgba16Unorm
            | Rgba8Snorm
    )
}

/// Procedural cube: 24 vertices (4 per face, wound CCW for back-face
/// culling), per-face colors and normals.
fn cube_geometry() -> (Vec<CubeVertex>, Vec<u16>) {
    let s = 0.5; // half-size; the model scale adjusts the world size

    let front = (0.0, 0.0, 1.0);
    let back = (0.0, 0.0, -1.0);
    let top = (0.0, 1.0, 0.0);
    let bottom = (0.0, -1.0, 0.0);
    let right = (1.0, 0.0, 0.0);
    let left = (-1.0, 0.0, 0.0);

    let c_front = [0.94, 0.38, 0.28]; // coral
    let c_back = [0.22, 0.62, 0.62]; // teal
    let c_top = [0.98, 0.80, 0.30]; // warm yellow
    let c_bottom = [0.45, 0.32, 0.68]; // purple
    let c_right = [0.95, 0.60, 0.24]; // orange
    let c_left = [0.36, 0.70, 0.90]; // sky blue

    let v = |p: [f32; 3], n: (f32, f32, f32), c: [f32; 3]| CubeVertex {
        position: p,
        normal: [n.0, n.1, n.2],
        color: c,
    };

    let vertices: Vec<CubeVertex> = vec![
        // front (z = +s)
        v([-s, -s, s], front, c_front),
        v([s, -s, s], front, c_front),
        v([s, s, s], front, c_front),
        v([-s, s, s], front, c_front),
        // back (z = -s)
        v([-s, -s, -s], back, c_back),
        v([-s, s, -s], back, c_back),
        v([s, s, -s], back, c_back),
        v([s, -s, -s], back, c_back),
        // top (y = +s)
        v([-s, s, -s], top, c_top),
        v([-s, s, s], top, c_top),
        v([s, s, s], top, c_top),
        v([s, s, -s], top, c_top),
        // bottom (y = -s)
        v([-s, -s, -s], bottom, c_bottom),
        v([s, -s, -s], bottom, c_bottom),
        v([s, -s, s], bottom, c_bottom),
        v([-s, -s, s], bottom, c_bottom),
        // right (x = +s)
        v([s, -s, -s], right, c_right),
        v([s, s, -s], right, c_right),
        v([s, s, s], right, c_right),
        v([s, -s, s], right, c_right),
        // left (x = -s)
        v([-s, -s, s], left, c_left),
        v([-s, s, s], left, c_left),
        v([-s, s, -s], left, c_left),
        v([-s, -s, -s], left, c_left),
    ];

    let indices: Vec<u16> = vec![
        0, 1, 2, 0, 2, 3, // front
        4, 5, 6, 4, 6, 7, // back
        8, 9, 10, 8, 10, 11, // top
        12, 13, 14, 12, 14, 15, // bottom
        16, 17, 18, 16, 18, 19, // right
        20, 21, 22, 20, 22, 23, // left
    ];

    (vertices, indices)
}
