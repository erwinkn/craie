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
        let instance = instance();
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::default(),
            compatible_surface: None,
            ..Default::default()
        }))
        .expect("no wgpu adapter");
        let (device, queue) = request_device(&adapter);
        Gpu {
            instance,
            adapter,
            device,
            queue,
        }
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

/// A configured surface ready to present frames.
pub struct WindowSurface {
    pub surface: Surface<'static>,
    pub config: SurfaceConfiguration,
}

impl WindowSurface {
    /// `size` is the surface size in physical pixels.
    pub fn new(gpu: &Gpu, surface: Surface<'static>, width: u32, height: u32) -> WindowSurface {
        let caps = surface.get_capabilities(&gpu.adapter);
        let format = caps
            .formats
            .iter()
            .copied()
            .find(|f| f.is_srgb())
            .unwrap_or(caps.formats[0]);
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
