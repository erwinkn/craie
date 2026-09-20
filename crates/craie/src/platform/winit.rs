//! Winit driver: owns the event loop, delivers Craie-level callbacks.
//!
//! Control flow is `Wait`: the loop sleeps until the OS or the app requests
//! work. `RedrawRequested` is the only place frames are produced.

use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::window::{WindowAttributes, WindowId};

use crate::geom::Size;
use crate::platform::{App, Wake, Window};

pub fn run<A: App>(title: &str, logical_size: Size, app: A) -> ! {
    let event_loop = EventLoop::new().expect("failed to create event loop");
    event_loop.set_control_flow(ControlFlow::Wait);
    let driver = Driver {
        app,
        window: None,
        wake: Wake {
            proxy: event_loop.create_proxy(),
        },
        attrs: WindowAttributes::default()
            .with_title(title)
            .with_surface_size(LogicalSize::new(
                logical_size.width as f64,
                logical_size.height as f64,
            )),
    };
    event_loop.run_app(driver).expect("event loop error");
    std::process::exit(0)
}

struct Driver<A: App> {
    app: A,
    window: Option<Window>,
    wake: Wake,
    attrs: WindowAttributes,
}

impl<A: App> ApplicationHandler for Driver<A> {
    fn can_create_surfaces(&mut self, event_loop: &dyn ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        let window = event_loop
            .create_window(self.attrs.clone())
            .expect("failed to create window");
        let window = Window::new(window);
        self.app.ready(&window, &self.wake);
        window.request_redraw();
        self.window = Some(window);
    }

    /// A `Wake::wake` arrived from another thread. The payload is implicit:
    /// the app drains whatever queues it owns.
    fn proxy_wake_up(&mut self, _event_loop: &dyn ActiveEventLoop) {
        if let Some(window) = &self.window {
            self.app.woke(window);
        }
    }

    fn window_event(
        &mut self,
        event_loop: &dyn ActiveEventLoop,
        _window_id: WindowId,
        event: WindowEvent,
    ) {
        let Some(window) = &self.window else { return };
        match event {
            WindowEvent::CloseRequested => {
                if self.app.close_requested() {
                    event_loop.exit();
                }
            }
            WindowEvent::SurfaceResized(_) | WindowEvent::ScaleFactorChanged { .. } => {
                self.app.resized(window);
            }
            WindowEvent::RedrawRequested => self.app.redraw(window),
            _ => {}
        }
    }
}
