//! wgpu bootstrap: instance, adapter, device, queue, surface.
//!
//! GPU objects are coarse resources — one device, one queue, a handful of
//! pipelines and buffers for the whole window. Nothing here is per-node.

use wgpu::{Adapter, Device, Instance, Queue, Surface, SurfaceConfiguration};

pub struct Gpu {
    pub instance: Instance,
    pub adapter: Adapter,
    pub device: Device,
    pub queue: Queue,
}

impl Gpu {
    /// Headless device for offscreen rendering and tests.
    pub fn headless() -> Gpu {
        Gpu::try_headless().expect("no wgpu adapter")
    }

    /// Headless device, or `None` when the machine has no adapter.
    /// GPU tests skip on `None`; with `CRAIE_REQUIRE_GPU` set (CI) a
    /// missing adapter panics instead, so a runner that lost its driver
    /// fails rather than skipping every GPU test.
    pub fn try_headless() -> Option<Gpu> {
        let instance = instance();
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::default(),
            compatible_surface: None,
            ..Default::default()
        }));
        let adapter = match adapter {
            Ok(adapter) => adapter,
            Err(e) if std::env::var_os("CRAIE_REQUIRE_GPU").is_some() => {
                panic!("CRAIE_REQUIRE_GPU is set and there is no wgpu adapter: {e}")
            }
            Err(_) => return None,
        };
        let (device, queue) = request_device(&adapter);
        Some(Gpu {
            instance,
            adapter,
            device,
            queue,
        })
    }

    /// Device + surface for a platform window. `target` is any wgpu window
    /// handle; platform code wraps winit so this module never names it.
    pub fn for_window(target: impl Into<wgpu::SurfaceTarget<'static>>) -> (Gpu, Surface<'static>) {
        let instance = instance();
        let surface = instance
            .create_surface(target)
            .expect("failed to create surface");
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::default(),
            compatible_surface: Some(&surface),
            ..Default::default()
        }))
        .expect("no wgpu adapter for surface");
        let (device, queue) = request_device(&adapter);
        (
            Gpu {
                instance,
                adapter,
                device,
                queue,
            },
            surface,
        )
    }
}

fn instance() -> Instance {
    Instance::new(wgpu::InstanceDescriptor {
        backends: wgpu::Backends::PRIMARY,
        ..wgpu::InstanceDescriptor::new_without_display_handle_from_env()
    })
}

fn request_device(adapter: &Adapter) -> (Device, Queue) {
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("craie"),
        required_features: wgpu::Features::empty(),
        required_limits: wgpu::Limits::default(),
        experimental_features: wgpu::ExperimentalFeatures::disabled(),
        memory_hints: wgpu::MemoryHints::Performance,
        trace: wgpu::Trace::Off,
    }))
    .expect("failed to request device");
    // No logger is installed in examples; surface validation errors
    // directly instead of letting them vanish into `log`.
    device.on_uncaptured_error(std::sync::Arc::new(|e| {
        eprintln!("[wgpu] {e}");
    }));
    (device, queue)
}

/// How translucent colors and antialiased edges composite. Browsers blend
/// sRGB-encoded values, so a 16 % tint over a dark canvas reads as on the
/// web; linear light is physically right and reads stronger. The
/// renderer follows its target's format (`Renderer::new`): this picks it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Blending {
    /// sRGB-encoded, as browsers: a plain (non-sRGB) target.
    #[default]
    Srgb,
    /// Linear light: an *-srgb target, which encodes on store.
    Linear,
}

impl Blending {
    /// A target format for this blending among `formats` (a surface's
    /// capabilities, preferred first), else the first.
    pub fn pick(self, formats: &[wgpu::TextureFormat]) -> wgpu::TextureFormat {
        use wgpu::TextureFormat as F;
        let linear = self == Blending::Linear;
        let eight_bit = |f: &F| matches!(f.remove_srgb_suffix(), F::Bgra8Unorm | F::Rgba8Unorm);
        let fits = |f: &F| f.is_srgb() == linear;
        formats
            .iter()
            .find(|f| eight_bit(f) && fits(f))
            .or_else(|| formats.iter().find(|f| fits(f)))
            .copied()
            .unwrap_or(formats[0])
    }

    /// The offscreen target format for this blending (headless, tests).
    pub fn offscreen(self) -> wgpu::TextureFormat {
        match self {
            Blending::Srgb => wgpu::TextureFormat::Bgra8Unorm,
            Blending::Linear => wgpu::TextureFormat::Bgra8UnormSrgb,
        }
    }
}

/// A configured surface ready to present frames.
pub struct WindowSurface {
    pub surface: Surface<'static>,
    pub config: SurfaceConfiguration,
}

impl WindowSurface {
    /// `size` is the surface size in physical pixels; `blending` picks
    /// its format (`Blending::pick`).
    pub fn new(
        gpu: &Gpu,
        surface: Surface<'static>,
        width: u32,
        height: u32,
        blending: Blending,
    ) -> WindowSurface {
        let caps = surface.get_capabilities(&gpu.adapter);
        let format = blending.pick(&caps.formats);
        let config = SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            width: width.max(1),
            height: height.max(1),
            present_mode: wgpu::PresentMode::AutoVsync,
            color_space: wgpu::SurfaceColorSpace::Auto,
            desired_maximum_frame_latency: 2,
            alpha_mode: wgpu::CompositeAlphaMode::Auto,
            view_formats: Vec::new(),
        };
        surface.configure(&gpu.device, &config);
        WindowSurface { surface, config }
    }

    /// Reconfigures after a resize. Zero extents are clamped; callers should
    /// also skip rendering while minimized.
    pub fn resize(&mut self, gpu: &Gpu, width: u32, height: u32) {
        if width == 0 || height == 0 {
            return;
        }
        self.config.width = width;
        self.config.height = height;
        self.surface.configure(&gpu.device, &self.config);
    }
}
