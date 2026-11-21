//! wgpu-direct rendering backend (the validated Phase 2 renderer,
//! generalized in Phase 3a to arbitrary box-part character models).
//!
//! Draws the [`CharacterModel`] as one painter's-order list of box parts
//! with a soft drop shadow over a fully transparent background, driven by
//! [`SceneState`] snapshots. The only `unsafe` is the documented
//! `create_surface_unsafe` call below — the same call Bevy's window
//! backend makes internally.
//!
//! Per-part transforms and tints are uniform slots written each frame;
//! the base geometry (unit cubes in model space, colored per part) is
//! static. One bind group per slot, no dynamic-offset complexity.

use ocm_character::model::{CharacterModel, ModelElement, VisualPart};
use ocm_character::{ElementHitbox, ElementPose};
use ocm_platform::NativeHandles;
use wgpu::util::DeviceExt;

use crate::scene::{self, Camera};
use crate::{FrameOutcome, Renderer, RendererError, SceneState};

/// Negotiated surface configuration (for logging/evidence).
#[derive(Debug, Clone)]
pub struct SurfaceInfo {
    pub adapter: String,
    pub backend: &'static str,
    pub format: wgpu::TextureFormat,
    pub alpha_mode: wgpu::CompositeAlphaMode,
    pub present_mode: wgpu::PresentMode,
}

/// Uniform slot layout, shared with `cube.wgsl` (WGSL uniform layout:
/// mat4 align 16, vec4 align 16 — matches this repr(C) struct).
#[repr(C)]
#[derive(Debug, Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct PartUniforms {
    view_proj: [[f32; 4]; 4],
    model: [[f32; 4]; 4],
    /// x/y/z: tint multiplier (base color × pose tint × hover), w: unused.
    params: [f32; 4],
}

/// The shadow slot interprets the same layout differently:
/// params.x = premultiplied-output flag, params.y = strength.
#[repr(C)]
#[derive(Debug, Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct ShadowUniforms {
    view_proj: [[f32; 4]; 4],
    model: [[f32; 4]; 4],
    params: [f32; 4],
}

/// 256-byte alignment keeps every slot usable as a uniform binding.
const UNIFORM_SLOT_BYTES: u64 = 256;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct PartVertex {
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

/// One drawable: an index range inside the shared part buffers, and its
/// uniform slot.
struct PartSlot {
    index_range: std::ops::Range<u32>,
    slot: u32,
}

/// The wgpu renderer.
pub struct WgpuRenderer {
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    premultiplied: bool,
    /// Clear color: fully transparent on compositing surfaces, the
    /// platform color key in the Opaque fallback.
    clear_color: wgpu::Color,
    uniform_buffer: wgpu::Buffer,
    /// One bind group per uniform slot (parts + the shadow last).
    bind_groups: Vec<wgpu::BindGroup>,
    part_pipeline: wgpu::RenderPipeline,
    shadow_pipeline: wgpu::RenderPipeline,
    parts: Vec<PartSlot>,
    shadow_slot: u32,
    part_vb: wgpu::Buffer,
    part_ib: wgpu::Buffer,
    shadow_vb: wgpu::Buffer,
    shadow_ib: wgpu::Buffer,
    shadow_index_count: u32,
    model: CharacterModel,
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
        opts: &crate::RenderOpts,
        model: &CharacterModel,
    ) -> Result<Self, RendererError> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: instance_backends(),
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
            .ok_or_else(|| RendererError::NoAlphaFormat(format!("{:?}", caps.formats)))?;

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
            .or_else(|| {
                // No compositing alpha available (Win32 DX12 HWND
                // surfaces report Opaque only): the documented color-key
                // fallback, only where the runtime asked for it.
                if opts.allow_opaque_fallback {
                    caps.alpha_modes
                        .iter()
                        .copied()
                        .find(|m| *m == wgpu::CompositeAlphaMode::Opaque)
                } else {
                    None
                }
            })
            .ok_or_else(|| RendererError::NoAlphaMode(format!("{:?}", caps.alpha_modes)))?;
        let opaque_fallback = alpha_mode == wgpu::CompositeAlphaMode::Opaque;
        let premultiplied = !matches!(alpha_mode, wgpu::CompositeAlphaMode::PostMultiplied);
        if opaque_fallback {
            tracing::warn!(
                "surface offers no compositing alpha; using the Opaque color-key fallback                  (soft shadow disabled, transparency is binary)"
            );
        } else if !premultiplied {
            tracing::warn!("compositor requires PostMultiplied alpha; shader emits straight alpha");
        }

        let clear_color = if opaque_fallback {
            wgpu::Color {
                r: opts.opaque_clear_color[0] as f64,
                g: opts.opaque_clear_color[1] as f64,
                b: opts.opaque_clear_color[2] as f64,
                a: 1.0,
            }
        } else {
            // (0,0,0,0) is both a valid premultiplied and straight-alpha
            // "empty" pixel.
            wgpu::Color {
                r: 0.0,
                g: 0.0,
                b: 0.0,
                a: 0.0,
            }
        };

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
            backend: match adapter_info.backend {
                wgpu::Backend::Vulkan => "Vulkan",
                wgpu::Backend::Dx12 => "Dx12",
                _ => "other",
            },
            format,
            alpha_mode,
            present_mode,
        };
        tracing::info!(?format, ?alpha_mode, ?present_mode, "surface configured");

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("character shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("cube.wgsl").into()),
        });

        // Uniform slots: one per drawable part, plus the shadow last.
        let drawable_parts: Vec<&VisualPart> = drawable_parts(model).collect();
        let slot_count = drawable_parts.len() as u64 + 1;
        let uniform_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("scene uniforms"),
            size: slot_count * UNIFORM_SLOT_BYTES,
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
                        std::mem::size_of::<PartUniforms>() as u64
                    ),
                },
                count: None,
            }],
        });
        let bind_groups: Vec<wgpu::BindGroup> = (0..slot_count)
            .map(|slot| {
                device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("scene bind group"),
                    layout: &bgl,
                    entries: &[wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                            buffer: &uniform_buffer,
                            offset: slot * UNIFORM_SLOT_BYTES,
                            size: wgpu::BufferSize::new(UNIFORM_SLOT_BYTES),
                        }),
                    }],
                })
            })
            .collect();

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("scene pipeline layout"),
            bind_group_layouts: &[Some(&bgl)],
            immediate_size: 0,
        });

        let part_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("character part pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_part"),
                buffers: &[Some(wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<PartVertex>() as u64,
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
                entry_point: Some("fs_part"),
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

        // Geometry: one unit-cube instance per drawable part (painter's
        // order = model declaration order), plus the shadow quad.
        let (part_vertices, part_indices, parts) = part_geometry(model);
        let part_vb = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("part vertex buffer"),
            contents: bytemuck::cast_slice(&part_vertices),
            usage: wgpu::BufferUsages::VERTEX,
        });
        let part_ib = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("part index buffer"),
            contents: bytemuck::cast_slice(&part_indices),
            usage: wgpu::BufferUsages::INDEX,
        });
        let shadow_vertices = [
            ShadowVertex {
                position: [-1.0, -1.0, 0.0],
                uv: [0.0, 0.0],
                _pad: [0.0; 3],
            },
            ShadowVertex {
                position: [1.0, -1.0, 0.0],
                uv: [1.0, 0.0],
                _pad: [0.0; 3],
            },
            ShadowVertex {
                position: [1.0, 1.0, 0.0],
                uv: [1.0, 1.0],
                _pad: [0.0; 3],
            },
            ShadowVertex {
                position: [-1.0, 1.0, 0.0],
                uv: [0.0, 1.0],
                _pad: [0.0; 3],
            },
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
            clear_color,
            uniform_buffer,
            bind_groups,
            part_pipeline,
            shadow_pipeline,
            parts,
            shadow_slot: (slot_count - 1) as u32,
            part_vb,
            part_ib,
            shadow_vb,
            shadow_ib,
            shadow_index_count,
            model: model.clone(),
            camera,
            logical,
            info,
        })
    }

    /// The negotiated surface configuration (evidence for the test suite).
    pub fn info(&self) -> &SurfaceInfo {
        &self.info
    }

    /// Effective tint for one part: the element's pose tint multiplied by
    /// hover brightening. The base color travels in the vertex attribute;
    /// the shader multiplies both.
    fn part_tint(
        &self,
        scene: &SceneState,
        element: &ModelElement,
        element_pose: &ElementPose,
        _part: &VisualPart,
    ) -> [f32; 3] {
        let mut tint = element_pose.tint;
        if scene.hover == Some(element.id) {
            // Gentle warm brightening, the wgpu-path successor of the
            // Phase 2 hover mix.
            tint = [
                (tint[0] * 1.22 + 0.02).min(1.0),
                (tint[1] * 1.16 + 0.015).min(1.0),
                (tint[2] * 1.06 + 0.01).min(1.0),
            ];
        }
        tint
    }
}

impl Renderer for WgpuRenderer {
    fn resize(&mut self, logical: (u32, u32), buffer: (u32, u32)) {
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

    fn render(&mut self, scene: &SceneState) -> Result<FrameOutcome, RendererError> {
        let frame = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(tex) => tex,
            wgpu::CurrentSurfaceTexture::Suboptimal(tex) => {
                tracing::debug!("surface texture suboptimal; continuing");
                tex
            }
            wgpu::CurrentSurfaceTexture::Timeout => {
                return Ok(FrameOutcome {
                    presented: false,
                    skipped: Some("timeout"),
                });
            }
            wgpu::CurrentSurfaceTexture::Occluded => {
                return Ok(FrameOutcome {
                    presented: false,
                    skipped: Some("occluded"),
                });
            }
            wgpu::CurrentSurfaceTexture::Outdated => {
                // Size state is driven by layer-shell configure events; the
                // next Configure/ScaleChanged will reconfigure. Retry soon.
                tracing::debug!("surface outdated; reconfiguring with last known size");
                self.surface.configure(&self.device, &self.config);
                return Ok(FrameOutcome {
                    presented: false,
                    skipped: Some("outdated"),
                });
            }
            wgpu::CurrentSurfaceTexture::Lost => {
                return Err(RendererError::SurfaceLost("surface lost".into()));
            }
            wgpu::CurrentSurfaceTexture::Validation => {
                return Err(RendererError::Render("surface validation error".into()));
            }
        };

        let placement = self.camera.placement(scene);
        let root = scene::root_model(&placement, &scene.pose.root);
        let shadow = scene::shadow_model(&placement, &scene.pose.root);

        // Write the per-part uniform slots (painter's order = the same
        // order the parts were baked into the vertex/index buffers).
        let mut slot = 0u32;
        for element in self.model.elements() {
            let element_pose = scene.pose.element(element.id);
            for part in &element.parts {
                let model = scene::part_model(&root, element, element_pose, part);
                let tint = self.part_tint(scene, element, element_pose, part);
                let uniforms = PartUniforms {
                    view_proj: placement.view_proj,
                    model,
                    params: [tint[0], tint[1], tint[2], 0.0],
                };
                self.queue.write_buffer(
                    &self.uniform_buffer,
                    slot as u64 * UNIFORM_SLOT_BYTES,
                    bytemuck::bytes_of(&uniforms),
                );
                slot += 1;
            }
        }

        // The shadow slot (last).
        let shadow_uniforms = ShadowUniforms {
            view_proj: placement.view_proj,
            model: shadow,
            params: [
                if self.premultiplied { 1.0 } else { 0.0 },
                scene::shadow_strength(&scene.pose.root),
                0.0,
                0.0,
            ],
        };
        self.queue.write_buffer(
            &self.uniform_buffer,
            self.shadow_slot as u64 * UNIFORM_SLOT_BYTES,
            bytemuck::bytes_of(&shadow_uniforms),
        );

        let view = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());

        let clear_color = self.clear_color;
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
                        // Transparent clear on alpha-compositing surfaces;
                        // the platform color key in the Opaque fallback.
                        load: wgpu::LoadOp::Clear(clear_color),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });

            // Painter's order: shadow first, parts over it. No blending —
            // each fragment computes its final alpha-mode-correct color.
            // The soft shadow is alpha-composited and cannot survive
            // binary color keying: it is skipped in the Opaque fallback.
            if self.has_per_pixel_alpha() {
                pass.set_pipeline(&self.shadow_pipeline);
                pass.set_bind_group(0, &self.bind_groups[self.shadow_slot as usize], &[]);
                pass.set_vertex_buffer(0, self.shadow_vb.slice(..));
                pass.set_index_buffer(self.shadow_ib.slice(..), wgpu::IndexFormat::Uint16);
                pass.draw_indexed(0..self.shadow_index_count, 0, 0..1);
            }

            pass.set_pipeline(&self.part_pipeline);
            pass.set_vertex_buffer(0, self.part_vb.slice(..));
            pass.set_index_buffer(self.part_ib.slice(..), wgpu::IndexFormat::Uint16);
            for part in &self.parts {
                pass.set_bind_group(0, &self.bind_groups[part.slot as usize], &[]);
                pass.draw_indexed(part.index_range.clone(), 0, 0..1);
            }
        }

        self.queue.submit(std::iter::once(encoder.finish()));
        // Queue::present attaches the swapchain image (dmabuf) to the
        // wl_surface and commits it — the commit that also applies any
        // pending input region / viewport state set before this render.
        // (wgpu 30 moved present from SurfaceTexture to Queue.)
        self.queue.present(frame);

        Ok(FrameOutcome {
            presented: true,
            skipped: None,
        })
    }

    fn character_hitboxes(&self, scene: &SceneState) -> Vec<ElementHitbox> {
        scene::element_hitboxes(scene, &self.model, &self.camera)
    }

    fn has_per_pixel_alpha(&self) -> bool {
        self.config.alpha_mode != wgpu::CompositeAlphaMode::Opaque
    }
}

/// GPU backends the renderer may use: Vulkan on every platform; DX12 is
/// added on Windows (native, and what most Windows machines have). The
/// instance is created with exactly these so adapter selection cannot
/// drift into an untested backend.
pub(crate) fn instance_backends() -> wgpu::Backends {
    #[cfg(windows)]
    {
        wgpu::Backends::VULKAN | wgpu::Backends::DX12
    }
    #[cfg(not(windows))]
    {
        wgpu::Backends::VULKAN
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

/// All drawable visual parts in painter's order (model declaration order).
fn drawable_parts(model: &CharacterModel) -> impl Iterator<Item = &VisualPart> {
    model.elements().iter().flat_map(|e| e.parts.iter())
}

/// One unit cube per drawable part, colored with the part's base color;
/// the per-frame model matrix does the placement/scaling. Returns the
/// vertex/index buffers and the per-part index ranges + slot indices.
fn part_geometry(model: &CharacterModel) -> (Vec<PartVertex>, Vec<u16>, Vec<PartSlot>) {
    let mut vertices = Vec::new();
    let mut indices = Vec::new();
    let mut parts = Vec::new();
    let mut slot = 0u32;
    for element in model.elements() {
        for part in &element.parts {
            let base = vertices.len() as u16;
            for v in unit_cube(part.color) {
                vertices.push(v);
            }
            for i in CUBE_INDICES {
                indices.push(base + i);
            }
            parts.push(PartSlot {
                index_range: (indices.len() - CUBE_INDICES.len()) as u32..(indices.len() as u32),
                slot,
            });
            slot += 1;
        }
    }
    (vertices, indices, parts)
}

const CUBE_INDICES: [u16; 36] = [
    0, 1, 2, 0, 2, 3, // front
    4, 5, 6, 4, 6, 7, // back
    8, 9, 10, 8, 10, 11, // top
    12, 13, 14, 12, 14, 15, // bottom
    16, 17, 18, 16, 18, 19, // right
    20, 21, 22, 20, 22, 23, // left
];

/// Unit cube spanning [-0.5, 0.5]³: 24 vertices (4 per face, wound CCW for
/// back-face culling) with per-face normals and one shared color.
fn unit_cube(color: [f32; 3]) -> Vec<PartVertex> {
    let s = 0.5;
    let v = |p: [f32; 3], n: [f32; 3]| PartVertex {
        position: p,
        normal: n,
        color,
    };
    let front = [0.0, 0.0, 1.0];
    let back = [0.0, 0.0, -1.0];
    let top = [0.0, 1.0, 0.0];
    let bottom = [0.0, -1.0, 0.0];
    let right = [1.0, 0.0, 0.0];
    let left = [-1.0, 0.0, 0.0];

    vec![
        // front (z = +s)
        v([-s, -s, s], front),
        v([s, -s, s], front),
        v([s, s, s], front),
        v([-s, s, s], front),
        // back (z = -s)
        v([-s, -s, -s], back),
        v([-s, s, -s], back),
        v([s, s, -s], back),
        v([s, -s, -s], back),
        // top (y = +s)
        v([-s, s, -s], top),
        v([-s, s, s], top),
        v([s, s, s], top),
        v([s, s, -s], top),
        // bottom (y = -s)
        v([-s, -s, -s], bottom),
        v([s, -s, -s], bottom),
        v([s, -s, s], bottom),
        v([-s, -s, s], bottom),
        // right (x = +s)
        v([s, -s, -s], right),
        v([s, s, -s], right),
        v([s, s, s], right),
        v([s, -s, s], right),
        // left (x = -s)
        v([-s, -s, s], left),
        v([-s, s, s], left),
        v([-s, s, -s], left),
        v([-s, -s, -s], left),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use ocm_character::prototype_cube;

    #[test]
    fn part_geometry_has_one_cube_per_drawable_part() {
        let model = prototype_cube();
        let drawable = drawable_parts(&model).count();
        let (vertices, indices, parts) = part_geometry(&model);
        assert_eq!(drawable, 7); // body + 2 arms + face plate + mouth + 2 eyes
        assert_eq!(vertices.len(), drawable * 24);
        assert_eq!(indices.len(), drawable * 36);
        assert_eq!(parts.len(), drawable);
        for (index, part) in parts.iter().enumerate() {
            assert_eq!(part.slot as usize, index);
            assert_eq!(part.index_range.len() as u32, 36);
        }
        assert!(indices.iter().all(|&i| (i as usize) < vertices.len()));
    }

    #[test]
    fn format_has_alpha_accepts_the_unorm_family() {
        for f in [
            wgpu::TextureFormat::Rgba8Unorm,
            wgpu::TextureFormat::Bgra8Unorm,
            wgpu::TextureFormat::Rgba16Float,
        ] {
            assert!(format_has_alpha(f));
        }
        assert!(!format_has_alpha(wgpu::TextureFormat::R8Unorm));
    }
}
