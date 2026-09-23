//! wgpu renderer for the retained scene.
//!
//! The scene's primitive pools and tables are mirrored in GPU storage
//! buffers. `prepare` uploads only the ranges the scene marked dirty
//! (nothing, for an unchanged frame); `draw` walks the scene's derived
//! draw list: one instanced draw per run of same-kind primitives, and an
//! isolated offscreen pass per opacity layer, composited with the
//! layer's opacity.
//!
//! The renderer targets a supplied device and render target, so a host
//! can embed it.

pub mod context;

mod atlas_gpu;
mod pipelines;

pub use context::{Gpu, WindowSurface};

use std::ops::Range;

use bytemuck::{Pod, Zeroable};
use craie_core::RectPx;
use craie_scene::{DrawCmd, RasterAtlas, Scene};
use wgpu::{Buffer, BufferUsages, TextureView};

/// Uniform shared by the scene and composite pipelines. 64 bytes; one
/// entry per target and per composite, at a 256-byte stride.
#[repr(C)]
#[derive(Clone, Copy, Default, Pod, Zeroable)]
struct Viewport {
    size: [f32; 2],
    origin: [f32; 2],
    scale: f32,
    page: f32,
    opacity: f32,
    _pad0: f32,
    rect: [f32; 4],
    uv: [f32; 2],
    _pad1: [f32; 2],
}

const VIEWPORT_STRIDE: u64 = 256;

fn srgb_to_linear(v: f64) -> f64 {
    if v <= 0.04045 {
        v / 12.92
    } else {
        ((v + 0.055) / 1.055).powf(2.4)
    }
}

/// A storage buffer mirroring one CPU table.
struct Mirror {
    label: &'static str,
    buf: Option<Buffer>,
    cap: u64,
}

impl Mirror {
    fn new(label: &'static str) -> Mirror {
        Mirror {
            label,
            buf: None,
            cap: 0,
        }
    }

    /// Uploads dirty `ranges` (in items) of `data`, or everything when the
    /// buffer must grow. Returns (bytes uploaded, buffer recreated).
    fn sync<T: Pod>(&mut self, gpu: &Gpu, data: &[T], ranges: &[Range<usize>]) -> (u64, bool) {
        let item = size_of::<T>() as u64;
        let needed = (data.len() as u64 * item).max(256);
        let mut recreated = false;
        if self.buf.is_none() || needed > self.cap {
            self.cap = needed.next_power_of_two();
            self.buf = Some(gpu.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(self.label),
                size: self.cap,
                usage: BufferUsages::STORAGE | BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }));
            recreated = true;
        }
        let buf = self.buf.as_ref().unwrap();
        let mut bytes = 0;
        if recreated {
            if !data.is_empty() {
                gpu.queue.write_buffer(buf, 0, bytemuck::cast_slice(data));
                bytes += data.len() as u64 * item;
            }
            return (bytes, true);
        }
        for r in ranges {
            let r = r.start.min(data.len())..r.end.min(data.len());
            if r.is_empty() {
                continue;
            }
            gpu.queue.write_buffer(
                buf,
                r.start as u64 * item,
                bytemuck::cast_slice(&data[r.clone()]),
            );
            bytes += r.len() as u64 * item;
        }
        (bytes, false)
    }

    fn binding(&self) -> wgpu::BindingResource<'_> {
        self.buf.as_ref().unwrap().as_entire_binding()
    }
}

/// GPU side of the raster atlas: two texture arrays plus the bind group.
struct AtlasGpu {
    alpha: wgpu::Texture,
    alpha_cap: u32,
    color: wgpu::Texture,
    color_cap: u32,
    bind_group: wgpu::BindGroup,
}

/// A pooled offscreen target for an opacity layer.
struct LayerTarget {
    w: u32,
    h: u32,
    view: TextureView,
    /// Composites this target; rebuilt when the uniform buffer is.
    composite: wgpu::BindGroup,
}

/// Per-frame planning buffers, kept so a frame allocates nothing.
#[derive(Default)]
struct DrawScratch {
    vps: Vec<Viewport>,
    /// Per layer: (target, viewport, composite viewport).
    plan: Vec<(usize, usize, usize)>,
    in_use: Vec<usize>,
    stack: Vec<usize>,
    parents: Vec<usize>,
    bytes: Vec<u8>,
}

/// Per-frame work counters.
#[derive(Clone, Copy, Debug, Default)]
pub struct RenderStats {
    pub draw_calls: u32,
    pub passes: u32,
    pub layers: u32,
    /// Bytes uploaded by the last `prepare`.
    pub upload_bytes: u64,
}

pub struct Renderer {
    format: wgpu::TextureFormat,
    scene_pipeline: wgpu::RenderPipeline,
    composite_pipeline: wgpu::RenderPipeline,
    scene_bgl: wgpu::BindGroupLayout,
    atlas_bgl: wgpu::BindGroupLayout,
    composite_bgl: wgpu::BindGroupLayout,
    atlas_sampler: wgpu::Sampler,
    layer_sampler: wgpu::Sampler,
    viewports: Buffer,
    viewport_cap: u64,
    rects: Mirror,
    glyphs: Mirror,
    paints: Mirror,
    placements: Mirror,
    worlds: Mirror,
    clips: Mirror,
    rasters: Mirror,
    scene_bg: Option<wgpu::BindGroup>,
    atlas: Option<AtlasGpu>,
    layer_pool: Vec<LayerTarget>,
    /// Viewport uniform bytes last written: an unchanged frame writes none.
    viewport_bytes: Vec<u8>,
    scratch: DrawScratch,
    /// Dirty ranges of the table being synced (reused).
    ranges: Vec<Range<usize>>,
    pub stats: RenderStats,
    /// Bytes uploaded to atlas textures this session (diagnostics).
    pub atlas_upload_bytes: u64,
}

impl Renderer {
    pub fn new(gpu: &Gpu, format: wgpu::TextureFormat) -> Renderer {
        let p = pipelines::build(gpu, format);
        let atlas_sampler = gpu.device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("atlas"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        // Layers composite 1:1 onto the device-pixel grid.
        let layer_sampler = gpu.device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("layer"),
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            ..Default::default()
        });
        let viewport_cap = VIEWPORT_STRIDE * 16;
        let viewports = gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("viewports"),
            size: viewport_cap,
            usage: BufferUsages::UNIFORM | BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        Renderer {
            format,
            scene_pipeline: p.scene,
            composite_pipeline: p.composite,
            scene_bgl: p.scene_bgl,
            atlas_bgl: p.atlas_bgl,
            composite_bgl: p.composite_bgl,
            atlas_sampler,
            layer_sampler,
            viewports,
            viewport_cap,
            rects: Mirror::new("rects"),
            glyphs: Mirror::new("glyphs"),
            paints: Mirror::new("paints"),
            placements: Mirror::new("placements"),
            worlds: Mirror::new("worlds"),
            clips: Mirror::new("clips"),
            rasters: Mirror::new("rasters"),
            scene_bg: None,
            atlas: None,
            layer_pool: Vec::new(),
            viewport_bytes: Vec::new(),
            scratch: DrawScratch::default(),
            ranges: Vec::new(),
            stats: RenderStats::default(),
            atlas_upload_bytes: 0,
        }
    }

    /// Uploads what the scene changed since the last call: dirty pool
    /// and table ranges, and dirty atlas rects. An unchanged scene
    /// uploads nothing. Adds the bytes to `scene.counters.upload_bytes`.
    pub fn prepare(&mut self, gpu: &Gpu, scene: &mut Scene) {
        let mut bytes = self.sync_atlas(gpu, &mut scene.atlas);
        let mut recreated = false;
        let mut add = |(b, r): (u64, bool)| {
            bytes += b;
            recreated |= r;
        };
        // One reused range buffer: a frame allocates nothing here.
        let mut r = std::mem::take(&mut self.ranges);
        scene.rects.take_dirty_into(&mut r);
        add(self.rects.sync(gpu, scene.rects.backing(), &r));
        scene.glyphs.take_dirty_into(&mut r);
        add(self.glyphs.sync(gpu, scene.glyphs.backing(), &r));
        scene.paints.take_dirty_into(&mut r);
        add(self.paints.sync(gpu, scene.paints.backing(), &r));
        scene.take_placement_dirty_into(&mut r);
        add(self.placements.sync(gpu, scene.placements(), &r));
        scene.transforms.take_gpu_dirty_into(&mut r);
        add(self.worlds.sync(gpu, scene.transforms.gpu_rows(), &r));
        scene.clips.take_gpu_dirty_into(&mut r);
        add(self.clips.sync(gpu, scene.clips.gpu_rows(), &r));
        scene.atlas.take_gpu_dirty_into(&mut r);
        add(self.rasters.sync(gpu, scene.atlas.gpu_rows(), &r));
        self.ranges = r;
        if recreated || self.scene_bg.is_none() {
            self.rebind(gpu);
        }
        self.stats.upload_bytes = bytes;
        scene.counters.upload_bytes += bytes;
    }

    /// Uploads dirty atlas regions and grows the texture arrays when the
    /// CPU atlas gained pages. Returns bytes uploaded.
    fn sync_atlas(&mut self, gpu: &Gpu, atlas: &mut RasterAtlas) -> u64 {
        let alpha_pages = atlas.alpha_pages() as u32;
        let color_pages = atlas.color_pages() as u32;
        let page_size = atlas.page_size();
        let (alpha_cap, color_cap) = match &self.atlas {
            Some(a) => (a.alpha_cap, a.color_cap),
            None => (0, 0),
        };
        let grow_alpha = alpha_pages > alpha_cap;
        let grow_color = color_pages > color_cap;
        let mut bytes = 0;
        let fresh = self.atlas.is_none() || grow_alpha || grow_color;
        if fresh {
            let alpha_cap = alpha_pages.max(1).next_power_of_two().max(alpha_cap);
            let color_cap = color_pages.max(1).next_power_of_two().max(color_cap);
            let (alpha, alpha_view) = atlas_gpu::array_texture(
                &gpu.device,
                page_size,
                alpha_cap,
                wgpu::TextureFormat::R8Unorm,
                "alpha",
            );
            let (color, color_view) = atlas_gpu::array_texture(
                &gpu.device,
                page_size,
                color_cap,
                wgpu::TextureFormat::Rgba8Unorm,
                "color",
            );
            let bind_group = atlas_gpu::bind_group(
                &gpu.device,
                &self.atlas_bgl,
                &alpha_view,
                &color_view,
                &self.atlas_sampler,
            );
            self.atlas = Some(AtlasGpu {
                alpha,
                alpha_cap,
                color,
                color_cap,
                bind_group,
            });
        }
        // Fresh textures need every page's used region; otherwise only
        // the dirty rects go up.
        let Some(a) = self.atlas.as_ref() else {
            return 0;
        };
        for color in [false, true] {
            let pages = if color { color_pages } else { alpha_pages };
            let bpp = if color { 4 } else { 1 };
            let tex = if color { &a.color } else { &a.alpha };
            for p in 0..pages as usize {
                let (data, dirty) = atlas.page_bytes(color, p);
                let rect = if fresh {
                    atlas.page_used(color, p)
                } else {
                    dirty
                };
                if let Some(rect) = rect {
                    bytes += upload_rect(gpu, tex, page_size, p as u32, rect, data, bpp);
                }
            }
        }
        atlas.clear_page_dirty();
        self.atlas_upload_bytes += bytes;
        bytes
    }

    /// A pooled layer target of at least `w` x `h` that no enclosing
    /// layer is using. Sizes round up so nearby layers share targets
    /// across frames.
    fn layer_target(&mut self, gpu: &Gpu, w: u32, h: u32, busy: &[usize]) -> usize {
        let (w, h) = (w.div_ceil(128) * 128, h.div_ceil(128) * 128);
        if let Some(i) = (0..self.layer_pool.len())
            .find(|&i| self.layer_pool[i].w == w && self.layer_pool[i].h == h && !busy.contains(&i))
        {
            return i;
        }
        let tex = gpu.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("layer"),
            size: wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: self.format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let view = tex.create_view(&Default::default());
        let composite = self.composite_bind_group(gpu, &view);
        self.layer_pool.push(LayerTarget {
            w,
            h,
            view,
            composite,
        });
        self.layer_pool.len() - 1
    }

    fn composite_bind_group(&self, gpu: &Gpu, view: &TextureView) -> wgpu::BindGroup {
        gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("composite"),
            layout: &self.composite_bgl,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                        buffer: &self.viewports,
                        offset: 0,
                        size: wgpu::BufferSize::new(size_of::<Viewport>() as u64),
                    }),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(&self.layer_sampler),
                },
            ],
        })
    }

    /// Encodes and submits the scene's draw list into `view`, a target of
    /// `width` x `height` device pixels. Call `prepare` first.
    /// Uniform bytes written here count in `scene.counters.upload_bytes`.
    pub fn draw(
        &mut self,
        gpu: &Gpu,
        view: &TextureView,
        width: u32,
        height: u32,
        scene: &mut Scene,
    ) {
        self.plan_frame(gpu, width, height, scene);
        self.encode_frame(gpu, view, scene);
    }

    /// Craie's share of `draw`: plans the layer targets and viewports and
    /// writes the viewport uniform when it changed. Once warm it
    /// allocates nothing (planning buffers are kept).
    pub fn plan_frame(&mut self, gpu: &Gpu, width: u32, height: u32, scene: &mut Scene) {
        let mut uniform_bytes = 0u64;
        let cmds = &scene.draw_list().cmds;
        let page = scene.atlas.page_size() as f32;
        let base = Viewport {
            size: [width as f32, height as f32],
            scale: scene.scale,
            page,
            ..Viewport::default()
        };

        // Plan: one viewport entry for the window, and per layer one for
        // its target plus one for its composite. Layer targets come from
        // the pool; a layer nested inside another needs its own target.
        let mut sc = std::mem::take(&mut self.scratch);
        let DrawScratch {
            vps,
            plan,
            in_use,
            stack,
            parents,
            bytes,
        } = &mut sc;
        vps.clear();
        plan.clear();
        in_use.clear();
        stack.clear();
        parents.clear();
        vps.push(base);
        parents.push(0);
        for cmd in cmds {
            match *cmd {
                DrawCmd::BeginLayer { opacity, bounds } => {
                    let w = (bounds[2] - bounds[0]).max(1) as u32;
                    let h = (bounds[3] - bounds[1]).max(1) as u32;
                    // A target is busy while an enclosing layer draws to it.
                    let t = self.layer_target(gpu, w, h, in_use);
                    in_use.push(t);
                    let (tw, th) = (self.layer_pool[t].w as f32, self.layer_pool[t].h as f32);
                    let vp = vps.len();
                    vps.push(Viewport {
                        size: [tw, th],
                        origin: [bounds[0] as f32, bounds[1] as f32],
                        ..base
                    });
                    let parent = *parents.last().unwrap();
                    let comp = vps.len();
                    vps.push(Viewport {
                        rect: [
                            bounds[0] as f32,
                            bounds[1] as f32,
                            bounds[2] as f32,
                            bounds[3] as f32,
                        ],
                        uv: [w as f32 / tw, h as f32 / th],
                        opacity,
                        ..vps[parent]
                    });
                    plan.push((t, vp, comp));
                    stack.push(plan.len() - 1);
                    parents.push(vp);
                }
                DrawCmd::EndLayer => {
                    let l = stack.pop().unwrap();
                    in_use.retain(|&t| t != plan[l].0);
                    parents.pop();
                }
                _ => {}
            }
        }
        let needed = vps.len() as u64 * VIEWPORT_STRIDE;
        let rebound = needed > self.viewport_cap;
        if rebound {
            self.viewport_cap = needed.next_power_of_two();
            self.viewports = gpu.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("viewports"),
                size: self.viewport_cap,
                usage: BufferUsages::UNIFORM | BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            // Rebuild the bind groups against the new uniform buffer.
            self.rebind(gpu);
            for i in 0..self.layer_pool.len() {
                let bg = self.composite_bind_group(gpu, &self.layer_pool[i].view);
                self.layer_pool[i].composite = bg;
            }
        }
        bytes.clear();
        bytes.resize(vps.len() * VIEWPORT_STRIDE as usize, 0);
        for (i, v) in vps.iter().enumerate() {
            let at = i * VIEWPORT_STRIDE as usize;
            bytes[at..at + size_of::<Viewport>()].copy_from_slice(bytemuck::bytes_of(v));
        }
        if *bytes != self.viewport_bytes || rebound {
            gpu.queue.write_buffer(&self.viewports, 0, bytes);
            self.stats.upload_bytes += bytes.len() as u64;
            uniform_bytes += bytes.len() as u64;
            std::mem::swap(bytes, &mut self.viewport_bytes);
        }
        self.scratch = sc;
        scene.counters.upload_bytes += uniform_bytes;
    }

    /// wgpu's share of `draw`: records the passes planned by
    /// `plan_frame` and submits them. Its allocations are wgpu's own
    /// (encoder, passes, submission), a fixed amount per pass.
    pub fn encode_frame(&mut self, gpu: &Gpu, view: &TextureView, scene: &Scene) {
        self.stats.draw_calls = 0;
        self.stats.passes = 0;
        self.stats.layers = 0;
        let sc = std::mem::take(&mut self.scratch);
        let cmds = &scene.draw_list().cmds;
        let mut encoder = gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("craie frame"),
            });
        let clear = scene.clear;
        let clear = wgpu::Color {
            r: srgb_to_linear(((clear.0 >> 24) & 0xff) as f64 / 255.0),
            g: srgb_to_linear(((clear.0 >> 16) & 0xff) as f64 / 255.0),
            b: srgb_to_linear(((clear.0 >> 8) & 0xff) as f64 / 255.0),
            a: (clear.0 & 0xff) as f64 / 255.0,
        };
        let mut layer_ix = 0usize;
        self.encode(
            &mut encoder,
            cmds,
            &mut 0,
            view,
            0,
            wgpu::LoadOp::Clear(clear),
            &sc.plan,
            &mut layer_ix,
        );
        self.scratch = sc;
        gpu.queue.submit([encoder.finish()]);
    }

    fn rebind(&mut self, gpu: &Gpu) {
        if self.rects.buf.is_none() {
            return;
        }
        self.scene_bg = Some(gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("scene"),
            layout: &self.scene_bgl,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                        buffer: &self.viewports,
                        offset: 0,
                        size: wgpu::BufferSize::new(size_of::<Viewport>() as u64),
                    }),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: self.rects.binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: self.glyphs.binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: self.paints.binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: self.placements.binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 5,
                    resource: self.worlds.binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 6,
                    resource: self.clips.binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 7,
                    resource: self.rasters.binding(),
                },
            ],
        }));
    }

    /// Encodes commands from `*i` into `target` until the matching
    /// `EndLayer` (or the end). Each layer becomes its own pass, then a
    /// composite into the reopened parent pass.
    #[allow(clippy::too_many_arguments)]
    fn encode(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        cmds: &[DrawCmd],
        i: &mut usize,
        target: &TextureView,
        vp: usize,
        mut load: wgpu::LoadOp<wgpu::Color>,
        plan: &[(usize, usize, usize)],
        layer_ix: &mut usize,
    ) {
        loop {
            // One pass on `target`, until a layer boundary.
            let mut boundary: Option<DrawCmd> = None;
            {
                let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("craie"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: target,
                        resolve_target: None,
                        depth_slice: None,
                        ops: wgpu::Operations {
                            load,
                            store: wgpu::StoreOp::Store,
                        },
                    })],
                    depth_stencil_attachment: None,
                    occlusion_query_set: None,
                    timestamp_writes: None,
                    multiview_mask: None,
                });
                self.stats.passes += 1;
                let mut bound = false;
                while *i < cmds.len() {
                    let cmd = cmds[*i];
                    match cmd {
                        DrawCmd::Rects { start, count } | DrawCmd::Glyphs { start, count } => {
                            if !bound {
                                pass.set_pipeline(&self.scene_pipeline);
                                pass.set_bind_group(
                                    0,
                                    self.scene_bg.as_ref().unwrap(),
                                    &[(vp as u64 * VIEWPORT_STRIDE) as u32],
                                );
                                pass.set_bind_group(
                                    1,
                                    &self.atlas.as_ref().unwrap().bind_group,
                                    &[],
                                );
                                bound = true;
                            }
                            let kind = if matches!(cmd, DrawCmd::Glyphs { .. }) {
                                1u32 << 31
                            } else {
                                0
                            };
                            pass.draw(0..4, (kind | start)..(kind | (start + count)));
                            self.stats.draw_calls += 1;
                            *i += 1;
                        }
                        DrawCmd::BeginLayer { .. } | DrawCmd::EndLayer => {
                            boundary = Some(cmd);
                            break;
                        }
                    }
                }
            }
            match boundary {
                None => return,
                Some(DrawCmd::EndLayer) => {
                    *i += 1;
                    return;
                }
                Some(_) => {
                    // Render the layer into its own target, then composite.
                    *i += 1;
                    let l = *layer_ix;
                    *layer_ix += 1;
                    self.stats.layers += 1;
                    let (t, layer_vp, comp_vp) = plan[l];
                    let view = self.layer_pool[t].view.clone();
                    self.encode(
                        encoder,
                        cmds,
                        i,
                        &view,
                        layer_vp,
                        wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                        plan,
                        layer_ix,
                    );
                    let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                        label: Some("composite"),
                        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                            view: target,
                            resolve_target: None,
                            depth_slice: None,
                            ops: wgpu::Operations {
                                load: wgpu::LoadOp::Load,
                                store: wgpu::StoreOp::Store,
                            },
                        })],
                        depth_stencil_attachment: None,
                        occlusion_query_set: None,
                        timestamp_writes: None,
                        multiview_mask: None,
                    });
                    self.stats.passes += 1;
                    pass.set_pipeline(&self.composite_pipeline);
                    pass.set_bind_group(
                        0,
                        &self.layer_pool[t].composite,
                        &[(comp_vp as u64 * VIEWPORT_STRIDE) as u32],
                    );
                    pass.draw(0..4, 0..1);
                    self.stats.draw_calls += 1;
                    load = wgpu::LoadOp::Load;
                }
            }
        }
    }
}

/// Uploads one rect of a CPU page mirror into a texture-array layer.
/// `Queue::write_texture` does not share the 256-byte row-alignment rule
/// of encoder copies, so the page mirror uploads in place: `offset` and
/// `bytes_per_row` address the rect directly. Returns bytes consumed.
fn upload_rect(
    gpu: &Gpu,
    texture: &wgpu::Texture,
    page_size: u32,
    layer: u32,
    rect: RectPx,
    page_data: &[u8],
    bpp: u32,
) -> u64 {
    let w = rect.max_x - rect.min_x;
    let h = rect.max_y - rect.min_y;
    if w == 0 || h == 0 {
        return 0;
    }
    let src_stride = (page_size * bpp) as usize;
    let offset = (rect.min_y as usize * src_stride + rect.min_x as usize * bpp as usize) as u64;
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
        page_data,
        wgpu::TexelCopyBufferLayout {
            offset,
            bytes_per_row: Some(page_size * bpp),
            rows_per_image: Some(h),
        },
        wgpu::Extent3d {
            width: w,
            height: h,
            depth_or_array_layers: 1,
        },
    );
    (page_size * bpp) as u64 * h as u64
}
