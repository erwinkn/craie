//! wgpu renderer: one pipeline, atlas textures, one instance buffer.
//!
//! Execution shape: a `Scene` (one ordered `Vec` of instances — quads and
//! glyphs share the format) is uploaded to a growable GPU buffer and drawn
//! in a single draw call, in document order, regardless of node count.

pub mod context;

mod atlas_gpu;
mod pipelines;

pub use context::{Gpu, WindowSurface};

use bytemuck::Pod;
use wgpu::{Buffer, BufferUsages, Device, Queue, TextureView};

use crate::geom::RectPx;
use crate::scene::Scene;
use crate::text::GlyphAtlas;

/// Uniform for the scene pipeline: surface size in physical pixels.
#[repr(C)]
#[derive(Clone, Copy, Pod, bytemuck::Zeroable)]
struct ViewportUniform {
    size: [f32; 2],
    _pad: [f32; 2],
}

fn atlas_tex_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable: true },
            view_dimension: wgpu::TextureViewDimension::D2Array,
            multisampled: false,
        },
        count: None,
    }
}

/// GPU-side mirror of the CPU atlas pages: two texture arrays plus the
/// bind group the glyph pipeline samples from.
struct AtlasGpu {
    alpha: wgpu::Texture,
    alpha_cap: u32,
    color: wgpu::Texture,
    color_cap: u32,
    bind_group: wgpu::BindGroup,
}

pub struct Renderer {
    pipeline: wgpu::RenderPipeline,
    viewport_buf: Buffer,
    viewport_bg: wgpu::BindGroup,
    atlas_layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    atlas: Option<AtlasGpu>,
    inst_buf: Option<Buffer>,
    inst_cap: u64,
    /// Bytes uploaded to atlas textures this session (diagnostics).
    pub atlas_upload_bytes: u64,
}

impl Renderer {
    pub fn new(gpu: &Gpu, surface_format: wgpu::TextureFormat) -> Renderer {
        let viewport_bgl = gpu
            .device
            .create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("viewport"),
                entries: &[wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                }],
            });

        let viewport_buf = gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("viewport"),
            size: 16,
            usage: BufferUsages::UNIFORM | BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let viewport_bg = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("viewport"),
            layout: &viewport_bgl,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: viewport_buf.as_entire_binding(),
            }],
        });

        let atlas_layout = gpu
            .device
            .create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("atlas"),
                entries: &[
                    atlas_tex_entry(0),
                    atlas_tex_entry(1),
                    wgpu::BindGroupLayoutEntry {
                        binding: 2,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                        count: None,
                    },
                ],
            });

        let sampler = gpu.device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("atlas"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });

        let pipeline = pipelines::scene(gpu, &viewport_bgl, &atlas_layout, surface_format);

        Renderer {
            pipeline,
            viewport_buf,
            viewport_bg,
            atlas_layout,
            sampler,
            atlas: None,
            inst_buf: None,
            inst_cap: 0,
            atlas_upload_bytes: 0,
        }
    }

    /// Updates the pixel→clip uniform. Called on resize/scale change.
    pub fn set_viewport(&self, gpu: &Gpu, width: f32, height: f32) {
        gpu.queue.write_buffer(
            &self.viewport_buf,
            0,
            bytemuck::bytes_of(&ViewportUniform {
                size: [width, height],
                _pad: [0.0; 2],
            }),
        );
    }

    /// Uploads dirty atlas regions and grows texture arrays when the CPU
    /// atlas gained pages. No-op when nothing changed.
    pub fn sync_atlas(&mut self, gpu: &Gpu, atlas: &mut GlyphAtlas) {
        let alpha_pages = atlas.alpha_pages() as u32;
        let color_pages = atlas.color_pages() as u32;

        let (alpha_cap, color_cap) = match &self.atlas {
            Some(a) => (a.alpha_cap, a.color_cap),
            None => (0, 0),
        };
        let grow_alpha = alpha_pages > alpha_cap;
        let grow_color = color_pages > color_cap;

        if self.atlas.is_none() || grow_alpha || grow_color {
            let new_alpha_cap = if grow_alpha {
                alpha_pages.next_power_of_two()
            } else {
                alpha_cap.max(1)
            };
            let new_color_cap = if grow_color {
                color_pages.next_power_of_two()
            } else {
                color_cap.max(1)
            };
            let (alpha, alpha_view) = atlas_gpu::array_texture(
                &gpu.device,
                crate::text::ATLAS_PAGE_SIZE,
                new_alpha_cap,
                wgpu::TextureFormat::R8Unorm,
                "alpha",
            );
            let (color, color_view) = atlas_gpu::array_texture(
                &gpu.device,
                crate::text::ATLAS_PAGE_SIZE,
                new_color_cap,
                wgpu::TextureFormat::Rgba8Unorm,
                "color",
            );
            let bind_group = atlas_gpu::bind_group(
                &gpu.device,
                &self.atlas_layout,
                &alpha_view,
                &color_view,
                &self.sampler,
            );
            self.atlas = Some(AtlasGpu {
                alpha,
                alpha_cap: new_alpha_cap,
                color,
                color_cap: new_color_cap,
                bind_group,
            });
            // Re-upload every page into the new textures.
            self.upload_all(gpu, atlas);
            return;
        }

        // Incremental path: only dirty rects.
        for color in [false, true] {
            let pages = if color { color_pages } else { alpha_pages };
            for p in 0..pages as usize {
                let (data, dirty) = atlas.page_bytes(color, p);
                let Some(rect) = dirty else { continue };
                let tex = if color {
                    &self.atlas.as_ref().unwrap().color
                } else {
                    &self.atlas.as_ref().unwrap().alpha
                };
                let bpp = if color { 4 } else { 1 };
                self.atlas_upload_bytes += Self::upload_rect(gpu, tex, p as u32, rect, data, bpp);
            }
        }
        atlas.clear_dirty();
    }

    /// Fresh texture arrays (first sync or growth) discard every prior
    /// upload, so each page pushes its `used` union — the allocated
    /// region, not the whole 2048² page.
    fn upload_all(&mut self, gpu: &Gpu, atlas: &mut GlyphAtlas) {
        let a = self.atlas.as_ref().unwrap();
        for p in 0..atlas.alpha_pages() {
            let (data, _) = atlas.page_bytes(false, p);
            if let Some(rect) = atlas.page_used(false, p) {
                self.atlas_upload_bytes += Self::upload_rect(gpu, &a.alpha, p as u32, rect, data, 1);
            }
        }
        for p in 0..atlas.color_pages() {
            let (data, _) = atlas.page_bytes(true, p);
            if let Some(rect) = atlas.page_used(true, p) {
                self.atlas_upload_bytes += Self::upload_rect(gpu, &a.color, p as u32, rect, data, 4);
            }
        }
        atlas.clear_dirty();
    }

    /// Uploads one rect of a CPU page mirror into a texture-array layer.
    /// Full-width rows are already 256-byte aligned in the page mirror and
    /// upload without repacking; narrower rects go through a padded staging
    /// copy. Returns bytes written.
    fn upload_rect(
        gpu: &Gpu,
        texture: &wgpu::Texture,
        layer: u32,
        rect: RectPx,
        page_data: &[u8],
        bpp: u32,
    ) -> u64 {
        let page_size = crate::text::ATLAS_PAGE_SIZE;
        let w = rect.max_x - rect.min_x;
        let h = rect.max_y - rect.min_y;
        if w == 0 || h == 0 {
            return 0;
        }
        let src_stride = (page_size * bpp) as usize;
        let owned;
        let (data, row_pitch): (&[u8], u32) = if w == page_size {
            let start = rect.min_y as usize * src_stride;
            (
                &page_data[start..start + h as usize * src_stride],
                page_size * bpp,
            )
        } else {
            owned = repack(page_data, src_stride, rect, bpp);
            (&owned, (w * bpp).next_multiple_of(256))
        };
        gpu.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture,
                mip_level: 0,
                origin: wgpu::Origin3d {
                    x: rect.min_x,
                    y: rect.min_y,
                    z: layer,
                },
                aspect: wgpu::TextureAspect::All,
            },
            data,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(row_pitch),
                rows_per_image: Some(h),
            },
            wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
        );
        (row_pitch as u64) * h as u64
    }

    /// Uploads instance data and draws the scene in one draw call.
    /// `view` is the frame's render target; `size` is in physical pixels.
    pub fn draw(&mut self, gpu: &Gpu, view: &TextureView, width: u32, height: u32, scene: &Scene) {
        self.set_viewport(gpu, width as f32, height as f32);
        self.inst_buf = write_instances(
            &gpu.device,
            &gpu.queue,
            self.inst_buf.take(),
            &mut self.inst_cap,
            &scene.items,
            "instances",
        );

        let mut encoder = gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("craie frame"),
            });
        {
            let clear = scene.clear.unwrap_or(crate::scene::Color::BLACK);
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("craie"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: ((clear.0 >> 24) & 0xff) as f64 / 255.0,
                            g: ((clear.0 >> 16) & 0xff) as f64 / 255.0,
                            b: ((clear.0 >> 8) & 0xff) as f64 / 255.0,
                            a: (clear.0 & 0xff) as f64 / 255.0,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                occlusion_query_set: None,
                timestamp_writes: None,
                multiview_mask: None,
            });
            if !scene.items.is_empty() {
                let atlas = self.atlas.as_ref().expect("scene drawn before atlas sync");
                pass.set_pipeline(&self.pipeline);
                pass.set_bind_group(0, &self.viewport_bg, &[]);
                pass.set_bind_group(1, &atlas.bind_group, &[]);
                pass.set_vertex_buffer(0, self.inst_buf.as_ref().unwrap().slice(..));
                pass.draw(0..4, 0..scene.items.len() as u32);
            }
        }
        gpu.queue.submit([encoder.finish()]);
    }
}

/// Copies a sub-rect out of a tightly packed page into a buffer whose rows
/// are padded to the 256-byte `write_texture` alignment.
fn repack(page: &[u8], stride: usize, rect: RectPx, bpp: u32) -> Vec<u8> {
    let w = (rect.max_x - rect.min_x) as usize;
    let h = (rect.max_y - rect.min_y) as usize;
    let row_bytes = w * bpp as usize;
    let padded = row_bytes.next_multiple_of(256);
    let mut out = vec![0u8; padded * h];
    for row in 0..h {
        let src = (rect.min_y as usize + row) * stride + rect.min_x as usize * bpp as usize;
        out[row * padded..row * padded + row_bytes].copy_from_slice(&page[src..src + row_bytes]);
    }
    out
}

/// Writes `instances` into a growable GPU buffer; recreates the buffer when
/// capacity is exceeded. Returns the live buffer.
fn write_instances<T: Pod>(
    device: &Device,
    queue: &Queue,
    buf: Option<Buffer>,
    cap: &mut u64,
    instances: &[T],
    label: &str,
) -> Option<Buffer> {
    let needed = std::mem::size_of_val(instances) as u64;
    let buf = match buf {
        Some(b) if needed <= *cap => b,
        _ => {
            *cap = needed.next_power_of_two().max(4096);
            device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(label),
                size: *cap,
                usage: BufferUsages::VERTEX | BufferUsages::COPY_DST,
                mapped_at_creation: false,
            })
        }
    };
    if needed > 0 {
        queue.write_buffer(&buf, 0, bytemuck::cast_slice(instances));
    }
    Some(buf)
}
