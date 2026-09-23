//! Pipeline and bind-group-layout construction.
//!
//! One scene pipeline serves every primitive kind: the instance index
//! selects a rect or a glyph from the storage-buffer pools, so a run of
//! draws never switches pipelines. A second pipeline composites opacity
//! layers.

use crate::Gpu;

pub struct Pipelines {
    pub scene: wgpu::RenderPipeline,
    pub composite: wgpu::RenderPipeline,
    pub scene_bgl: wgpu::BindGroupLayout,
    pub atlas_bgl: wgpu::BindGroupLayout,
    pub composite_bgl: wgpu::BindGroupLayout,
}

fn shader(device: &wgpu::Device, src: &str, label: &str) -> wgpu::ShaderModule {
    let full = format!("{}\n{}", include_str!("shaders/common.wgsl"), src);
    device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some(label),
        source: wgpu::ShaderSource::Wgsl(full.into()),
    })
}

fn viewport_entry(visibility: wgpu::ShaderStages) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding: 0,
        visibility,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Uniform,
            has_dynamic_offset: true,
            min_binding_size: None,
        },
        count: None,
    }
}

fn storage_entry(binding: u32, visibility: wgpu::ShaderStages) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Storage { read_only: true },
            has_dynamic_offset: false,
            min_binding_size: None,
        },
        count: None,
    }
}

fn texture_entry(binding: u32, dim: wgpu::TextureViewDimension) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable: true },
            view_dimension: dim,
            multisampled: false,
        },
        count: None,
    }
}

fn sampler_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
        count: None,
    }
}

fn pipeline(
    gpu: &Gpu,
    label: &str,
    module: &wgpu::ShaderModule,
    layouts: &[Option<&wgpu::BindGroupLayout>],
    format: wgpu::TextureFormat,
) -> wgpu::RenderPipeline {
    let layout = gpu
        .device
        .create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some(label),
            bind_group_layouts: layouts,
            immediate_size: 0,
        });
    gpu.device
        .create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some(label),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module,
                entry_point: Some("vs_main"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            fragment: Some(wgpu::FragmentState {
                module,
                entry_point: Some("fs_main"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    // All fragment outputs are premultiplied alpha.
                    blend: Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleStrip,
                ..Default::default()
            },
            depth_stencil: None,
            multisample: Default::default(),
            multiview_mask: None,
            cache: None,
        })
}

pub fn build(gpu: &Gpu, format: wgpu::TextureFormat) -> Pipelines {
    use wgpu::ShaderStages as S;
    let vf = S::VERTEX | S::FRAGMENT;
    let scene_bgl = gpu
        .device
        .create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("scene"),
            entries: &[
                viewport_entry(vf),
                storage_entry(1, S::VERTEX),
                storage_entry(2, S::VERTEX),
                storage_entry(3, S::VERTEX),
                storage_entry(4, S::VERTEX),
                storage_entry(5, S::VERTEX),
                storage_entry(6, S::FRAGMENT),
                storage_entry(7, S::VERTEX),
            ],
        });
    let atlas_bgl = gpu
        .device
        .create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("atlas"),
            entries: &[
                texture_entry(0, wgpu::TextureViewDimension::D2Array),
                texture_entry(1, wgpu::TextureViewDimension::D2Array),
                sampler_entry(2),
            ],
        });
    let composite_bgl = gpu
        .device
        .create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("composite"),
            entries: &[
                viewport_entry(vf),
                texture_entry(1, wgpu::TextureViewDimension::D2),
                sampler_entry(2),
            ],
        });
    let scene_module = shader(&gpu.device, include_str!("shaders/scene.wgsl"), "scene");
    let composite_module = shader(&gpu.device, include_str!("shaders/composite.wgsl"), "composite");
    Pipelines {
        scene: pipeline(gpu, "scene", &scene_module, &[Some(&scene_bgl), Some(&atlas_bgl)], format),
        composite: pipeline(gpu, "composite", &composite_module, &[Some(&composite_bgl)], format),
        scene_bgl,
        atlas_bgl,
        composite_bgl,
    }
}
