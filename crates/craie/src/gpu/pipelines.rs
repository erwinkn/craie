//! Render pipeline construction. One instanced pipeline serves every
//! drawable: rounded/bordered solid rects and atlas-sampled glyphs share
//! the 76-byte instance format, so a frame is a single ordered draw call.

use crate::gpu::Gpu;
use crate::scene::Instance;

fn shader(device: &wgpu::Device, src: &str, label: &str) -> wgpu::ShaderModule {
    device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some(label),
        source: wgpu::ShaderSource::Wgsl(src.into()),
    })
}

fn blend() -> wgpu::BlendState {
    // All fragment outputs are premultiplied alpha.
    wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING
}

fn primitive() -> wgpu::PrimitiveState {
    wgpu::PrimitiveState {
        topology: wgpu::PrimitiveTopology::TriangleStrip,
        ..Default::default()
    }
}

pub fn scene(
    gpu: &Gpu,
    viewport_bgl: &wgpu::BindGroupLayout,
    atlas_bgl: &wgpu::BindGroupLayout,
    format: wgpu::TextureFormat,
) -> wgpu::RenderPipeline {
    let module = shader(&gpu.device, include_str!("shaders/scene.wgsl"), "scene");
    let layout = gpu
        .device
        .create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("scene"),
            bind_group_layouts: &[Some(viewport_bgl), Some(atlas_bgl)],
            immediate_size: 0,
        });
    gpu.device
        .create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("scene"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &module,
                entry_point: Some("vs_main"),
                compilation_options: Default::default(),
                buffers: &[Some(wgpu::VertexBufferLayout {
                    array_stride: size_of::<Instance>() as u64,
                    step_mode: wgpu::VertexStepMode::Instance,
                    attributes: &wgpu::vertex_attr_array![
                        0 => Float32x2,  // position
                        1 => Float32x2,  // size
                        2 => Float32x2,  // uv_min
                        3 => Float32x2,  // uv_max
                        4 => Float32x2,  // clip_min
                        5 => Float32x2,  // clip_max
                        6 => Uint32,     // color
                        7 => Uint32,     // aux_color (border)
                        8 => Float32x4,  // params (radius, border_w, clip_r, _)
                        9 => Uint16x2,   // page, flags
                    ],
                })],
            },
            fragment: Some(wgpu::FragmentState {
                module: &module,
                entry_point: Some("fs_main"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(blend()),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: primitive(),
            depth_stencil: None,
            multisample: Default::default(),
            multiview_mask: None,
            cache: None,
        })
}
