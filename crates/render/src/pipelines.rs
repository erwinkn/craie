//! Pipeline and bind-group-layout construction.
//!
//! One scene pipeline serves rects and glyphs: the instance index
//! selects a rect or a glyph from the storage-buffer pools, so a run of
//! draws never switches pipelines. The path pipeline draws mesh
//! triangles from the same tables. A third pipeline composites opacity
//! layers. Each exists single-sampled and at `MSAA` samples: a frame
//! that draws paths renders multisampled.

/// Samples of a frame that draws paths.
pub const MSAA: u32 = 4;

use crate::Gpu;

/// Pipelines indexed by `[single, multisampled]`.
pub struct Pipelines {
    pub scene: [wgpu::RenderPipeline; 2],
    pub path: [wgpu::RenderPipeline; 2],
    pub composite: [wgpu::RenderPipeline; 2],
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

#[allow(clippy::too_many_arguments)]
fn pipeline(
    gpu: &Gpu,
    label: &str,
    module: &wgpu::ShaderModule,
    (vs, fs): (&str, &str),
    topology: wgpu::PrimitiveTopology,
    layouts: &[Option<&wgpu::BindGroupLayout>],
    format: wgpu::TextureFormat,
    samples: u32,
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
                entry_point: Some(vs),
                compilation_options: Default::default(),
                buffers: &[],
            },
            fragment: Some(wgpu::FragmentState {
                module,
                entry_point: Some(fs),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    // All fragment outputs are premultiplied alpha.
                    blend: Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState {
                topology,
                ..Default::default()
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState {
                count: samples,
                ..Default::default()
            },
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
                // Paints: fragments read gradient records.
                storage_entry(3, vf),
                storage_entry(4, S::VERTEX),
                storage_entry(5, S::VERTEX),
                storage_entry(6, S::FRAGMENT),
                storage_entry(7, S::VERTEX),
                storage_entry(8, S::VERTEX),
                storage_entry(9, S::VERTEX),
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
    let composite_module = shader(
        &gpu.device,
        include_str!("shaders/composite.wgsl"),
        "composite",
    );
    use wgpu::PrimitiveTopology::{TriangleList, TriangleStrip};
    let both = |label: &str,
                module: &wgpu::ShaderModule,
                entries: (&str, &str),
                topology,
                layouts: &[Option<&wgpu::BindGroupLayout>]| {
        [1, MSAA].map(|n| pipeline(gpu, label, module, entries, topology, layouts, format, n))
    };
    let scene_layouts = [Some(&scene_bgl), Some(&atlas_bgl)];
    Pipelines {
        scene: both(
            "scene",
            &scene_module,
            ("vs_main", "fs_main"),
            TriangleStrip,
            &scene_layouts,
        ),
        path: both(
            "path",
            &scene_module,
            ("vs_path", "fs_path"),
            TriangleList,
            &scene_layouts,
        ),
        composite: both(
            "composite",
            &composite_module,
            ("vs_main", "fs_main"),
            TriangleStrip,
            &[Some(&composite_bgl)],
        ),
        scene_bgl,
        atlas_bgl,
        composite_bgl,
    }
}
