use ocm_platform::SurfaceHandle;
use raw_window_handle::{RawDisplayHandle, RawWindowHandle};
use tracing;
use wgpu::util::DeviceExt;

pub struct SceneState {
    pub character_x: f32,
    pub character_y: f32,
    pub character_size: f32,
    pub rotation: f32,
    pub dirty: bool,
    pub animating: bool,
}

impl Default for SceneState {
    fn default() -> Self {
        Self {
            character_x: 0.0,
            character_y: 0.0,
            character_size: 200.0,
            rotation: 0.0,
            dirty: true,
            animating: true,
        }
    }
}

pub struct WgpuRenderer {
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    pipeline: wgpu::RenderPipeline,
    vertex_buffer: wgpu::Buffer,
    index_buffer: wgpu::Buffer,
    index_count: u32,
    depth_format: wgpu::TextureFormat,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Vertex {
    position: [f32; 3],
    color: [f32; 3],
}

impl WgpuRenderer {
    pub fn new(
        raw_window_handle: RawWindowHandle,
        raw_display_handle: RawDisplayHandle,
        width: i32,
        height: i32,
    ) -> Result<Self, String> {
        let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor {
            backend: wgpu::Backends::VULKAN,
            ..Default::default()
        });

        let surface = unsafe {
            instance.create_surface_unsafe(wgpu::SurfaceTargetUnsafe::RawHandle {
                raw_display_handle: Some(raw_display_handle),
                raw_window_handle,
            })
        }
        .map_err(|e| format!("surface creation failed: {e:?}"))?;

        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            compatible_surface: Some(&surface),
            force_fallback_adapter: false,
        }))
        .ok_or("no suitable GPU adapter found")?;

        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            required_features: wgpu::Features::empty(),
            required_limits: wgpu::Limits::default(),
            memory_hints: wgpu::MemoryHints::default(),
            trace: wgpu::Trace::Off,
        }))
        .map_err(|e| format!("device request failed: {e:?}"))?;

        let caps = surface.get_capabilities(&adapter);
        let format = caps
            .formats
            .iter()
            .find(|f| f.is_srgb())
            .copied()
            .unwrap_or(caps.formats[0]);
        let alpha_mode = caps
            .alpha_modes
            .iter()
            .find(|m| **m == wgpu::CompositeAlphaMode::Premultiplied)
            .copied()
            .unwrap_or(caps.alpha_modes[0]);
        let present_mode = caps
            .present_modes
            .iter()
            .find(|m| **m == wgpu::PresentMode::Fifo)
            .copied()
            .unwrap_or(caps.present_modes[0]);

        tracing::info!(
            "surface format: {:?}, alpha_mode: {:?}, present_mode: {:?}",
            format,
            alpha_mode,
            present_mode
        );

        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            color_space: wgpu::SurfaceColorSpace::Auto,
            width: width.max(1) as u32,
            height: height.max(1) as u32,
            present_mode,
            desired_maximum_frame_latency: 2,
            alpha_mode,
            view_formats: vec![],
        };

        surface.configure(&device, &config);

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("cube shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("cube.wgsl")),
        });

        let depth_format = wgpu::TextureFormat::Depth32Float;

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("cube pipeline"),
            layout: None,
            vertex: wgpu::VertexState {
                shader: &shader,
                entry_point: Some("vs_main"),
                buffers: &[wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<Vertex>() as wgpu::BufferAddress,
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
                            offset: std::mem::size_of::<[f32; 3]>() as wgpu::BufferAddress,
                        },
                    ],
                }],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                shader: &shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                cull_mode: Some(wgpu::Face::Back),
                ..Default::default()
            },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: depth_format,
                depth_write_enabled: true,
                depth_compare: wgpu::CompareFunction::Less,
                stencil: wgpu::StencilState::default(),
                bias: wgpu::DepthBiasState::default(),
            }),
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
            cache: None,
        });

        let (vertices, indices) = cube_geometry();
        let vertex_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("vertex buffer"),
            contents: bytemuck::cast_slice(&vertices),
            usage: wgpu::BufferUsages::VERTEX,
        });
        let index_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("index buffer"),
            contents: bytemuck::cast_slice(&indices),
            usage: wgpu::BufferUsages::INDEX,
        });

        Ok(Self {
            surface,
            device,
            queue,
            config,
            pipeline,
            vertex_buffer,
            index_buffer,
            index_count: indices.len() as u32,
            depth_format,
        })
    }

    pub fn resize(&mut self, width: u32, height: u32) {
        if width > 0 && height > 0 {
            self.config.width = width;
            self.config.height = height;
            self.surface.configure(&self.device, &self.config);
        }
    }

    pub fn render(&mut self, scene: &SceneState) -> Result<(), String> {
        let frame = self
            .surface
            .get_current_texture()
            .map_err(|e| format!("get_current_texture failed: {e:?}"))?;

        let view = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());

        let depth_texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("depth texture"),
            size: wgpu::Extent3d {
                width: self.config.width,
                height: self.config.height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: self.depth_format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        });
        let depth_view = depth_texture.create_view(&wgpu::TextureViewDescriptor::default());

        let aspect = self.config.width as f32 / self.config.height as f32;
        let mvp = build_mvp(aspect, scene);

        let uniform_buffer = self.device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("uniform buffer"),
            contents: bytemuck::cast_slice(&mvp),
            usage: wgpu::BufferUsages::UNIFORM,
        });

        let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("bind group"),
            layout: &self.pipeline.get_bind_group_layout(0),
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: uniform_buffer.as_entire_binding(),
            }],
        });

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("render encoder"),
            });

        {
            let mut render_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("render pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: 0.0,
                            g: 0.0,
                            b: 0.0,
                            a: 0.0,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &depth_view,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(1.0),
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });

            render_pass.set_pipeline(&self.pipeline);
            render_pass.set_bind_group(0, &bind_group, &[]);
            render_pass.set_vertex_buffer(0, self.vertex_buffer.slice(..));
            render_pass.set_index_buffer(self.index_buffer.slice(..), wgpu::IndexFormat::Uint16);
            render_pass.draw_indexed(0..self.index_count, 0, 0..1);
        }

        self.queue.submit(std::iter::once(encoder.finish()));
        frame.present();

        Ok(())
    }
}

fn build_mvp(aspect: f32, scene: &SceneState) -> [[f32; 4]; 4] {
    let scale = 2.0 / scene.character_size;
    let tx = scene.character_x * scale;
    let ty = scene.character_y * scale;
    let r = scene.rotation;

    let cos = r.cos();
    let sin = r.sin();

    let model = [
        [cos * scale, sin * scale, 0.0, 0.0],
        [-sin * scale, cos * scale, 0.0, 0.0],
        [0.0, 0.0, scale, 0.0],
        [tx, ty, 0.0, 1.0],
    ];

    let proj = ortho(aspect);
    mat4_mul(proj, model)
}

fn ortho(aspect: f32) -> [[f32; 4]; 4] {
    let l = -1.0 * aspect;
    let r = 1.0 * aspect;
    let b = -1.0;
    let t = 1.0;
    let n = 0.1;
    let f = 100.0;
    [
        [2.0 / (r - l), 0.0, 0.0, 0.0],
        [0.0, 2.0 / (t - b), 0.0, 0.0],
        [0.0, 0.0, -2.0 / (f - n), 0.0],
        [-(r + l) / (r - l), -(t + b) / (t - b), -(f + n) / (f - n), 1.0],
    ]
}

fn mat4_mul(a: [[f32; 4]; 4], b: [[f32; 4]; 4]) -> [[f32; 4]; 4] {
    let mut result = [[0.0f32; 4]; 4];
    for i in 0..4 {
        for j in 0..4 {
            let mut sum = 0.0;
            for k in 0..4 {
                sum += a[i][k] * b[k][j];
            }
            result[i][j] = sum;
        }
    }
    result
}

fn cube_geometry() -> (Vec<Vertex>, Vec<u16>) {
    let s = 0.5;
    let vertices = [
        Vertex { position: [-s, -s, s], color: [0.8, 0.2, 0.2] },
        Vertex { position: [s, -s, s], color: [0.8, 0.2, 0.2] },
        Vertex { position: [s, s, s], color: [0.8, 0.2, 0.2] },
        Vertex { position: [-s, s, s], color: [0.8, 0.2, 0.2] },
        Vertex { position: [-s, -s, -s], color: [0.2, 0.8, 0.2] },
        Vertex { position: [-s, s, -s], color: [0.2, 0.8, 0.2] },
        Vertex { position: [s, s, -s], color: [0.2, 0.8, 0.2] },
        Vertex { position: [s, -s, -s], color: [0.2, 0.8, 0.2] },
        Vertex { position: [-s, s, -s], color: [0.2, 0.2, 0.8] },
        Vertex { position: [-s, s, s], color: [0.2, 0.2, 0.8] },
        Vertex { position: [s, s, s], color: [0.2, 0.2, 0.8] },
        Vertex { position: [s, s, -s], color: [0.2, 0.2, 0.8] },
        Vertex { position: [-s, -s, -s], color: [0.8, 0.8, 0.2] },
        Vertex { position: [s, -s, -s], color: [0.8, 0.8, 0.2] },
        Vertex { position: [s, -s, s], color: [0.8, 0.8, 0.2] },
        Vertex { position: [-s, -s, s], color: [0.8, 0.8, 0.2] },
        Vertex { position: [s, -s, -s], color: [0.8, 0.2, 0.8] },
        Vertex { position: [s, s, -s], color: [0.8, 0.2, 0.8] },
        Vertex { position: [s, s, s], color: [0.8, 0.2, 0.8] },
        Vertex { position: [s, -s, s], color: [0.8, 0.2, 0.8] },
        Vertex { position: [-s, -s, s], color: [0.2, 0.8, 0.8] },
        Vertex { position: [-s, s, s], color: [0.2, 0.8, 0.8] },
        Vertex { position: [-s, s, -s], color: [0.2, 0.8, 0.8] },
        Vertex { position: [-s, -s, -s], color: [0.2, 0.8, 0.8] },
    ];

    let indices: Vec<u16> = vec![
        0, 1, 2, 0, 2, 3, 4, 5, 6, 4, 6, 7, 8, 9, 10, 8, 10, 11, 12, 13, 14, 12, 14, 15, 16, 17,
        18, 16, 18, 19, 20, 21, 22, 20, 22, 23,
    ];

    (vertices.to_vec(), indices)
}