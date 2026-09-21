//! Winit driver: owns the event loop, delivers Craie-level callbacks.
//!
//! Control flow is `Wait`: the loop sleeps until the OS or the app requests
//! work. `RedrawRequested` is the only place frames are produced.
//!
//! Input normalization lives here: winit window events become
//! `events::Event`s in logical points, so nothing above this module names
//! a winit type. Modifier state is tracked from `ModifiersChanged`.

use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
use winit::event::{ElementState, Ime, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{Key as WKey, NamedKey};
use winit::window::{WindowAttributes, WindowId};

use crate::events::{Button, Event, Key, KeyInput, Mods};
use crate::geom::Size;
use crate::platform::{App, Wake, Window};

pub fn run<A: App>(title: &str, logical_size: Size, app: A) {
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
            // Repo rule: windows never steal focus from the user.
            .with_active(false)
            // The AccessKit adapter must exist before first show.
            .with_visible(false)
            .with_surface_size(LogicalSize::new(
                logical_size.width as f64,
                logical_size.height as f64,
            )),
        mods: Mods::default(),
        pointer: (0.0, 0.0),
    };
    event_loop.run_app(driver).expect("event loop error");
}

struct Driver<A: App> {
    app: A,
    window: Option<Window>,
    wake: Wake,
    attrs: WindowAttributes,
    mods: Mods,
    /// Last pointer position (logical); wheel events carry no position.
    pointer: (f32, f32),
}

impl<A: App> Driver<A> {
    fn key_input(&self, event: &winit::event::KeyEvent) -> KeyInput {
        let key = match &event.logical_key {
            WKey::Named(named) => match named {
                NamedKey::Backspace => Key::Backspace,
                NamedKey::Tab => Key::Tab,
                NamedKey::Enter => Key::Enter,
                NamedKey::Escape => Key::Escape,
                NamedKey::ArrowLeft => Key::Left,
                NamedKey::ArrowUp => Key::Up,
                NamedKey::ArrowRight => Key::Right,
                NamedKey::ArrowDown => Key::Down,
                NamedKey::Home => Key::Home,
                NamedKey::End => Key::End,
                NamedKey::PageUp => Key::PageUp,
                NamedKey::PageDown => Key::PageDown,
                NamedKey::Delete => Key::Delete,
                _ => Key::Unknown,
            },
            _ => Key::Unknown,
        };
        let char = match &event.logical_key {
            WKey::Character(c) => Some(c.to_string()),
            _ => None,
        };
        // `text` is the printable string for this press. Command chords
        // (meta/ctrl) are shortcuts, not text — the platform filters them
        // out here rather than at the editor.
        let text = if self.mods.meta || self.mods.ctrl {
            None
        } else {
            event.text.as_ref().map(|t| t.to_string()).or_else(|| {
                (key == Key::Unknown).then(|| char.clone()).flatten()
            })
        };
        KeyInput {
            key,
            text,
            char,
            mods: self.mods,
        }
    }
}

impl<A: App> ApplicationHandler for Driver<A> {
    fn can_create_surfaces(&mut self, event_loop: &dyn ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        let window = event_loop
            .create_window(self.attrs.clone())
            .expect("failed to create window");
        let a11y = self.app.a11y_shared().map(|shared| {
            accesskit_winit::Adapter::with_direct_handlers(
                event_loop,
                &*window,
                crate::a11y::Activation {
                    shared: shared.clone(),
                },
                crate::a11y::ActionSink {
                    shared,
                    wake: self.wake.clone(),
                },
                crate::a11y::Deactivation,
            )
        });
        let window = Window::new(window, a11y);
        // Adapter in place: safe to show.
        window.inner.set_visible(true);
        self.app.ready(&window, &self.wake);
        window.request_redraw();
        self.window = Some(window);
    }

    /// A `Wake::wake` arrived from another thread. The payload is implicit:
    /// the app drains whatever queues it owns.
    fn proxy_wake_up(&mut self, event_loop: &dyn ActiveEventLoop) {
        if let Some(window) = &self.window
            && self.app.woke(window)
        {
            event_loop.exit();
        }
    }

    fn window_event(
        &mut self,
        event_loop: &dyn ActiveEventLoop,
        _window_id: WindowId,
        event: WindowEvent,
    ) {
        let Some(window) = &self.window else { return };
        window.process_a11y_event(&event);
        let scale = window.scale_factor() as f32;
        let logical = |x: f64, y: f64| ((x as f32) / scale, (y as f32) / scale);

        match event {
            WindowEvent::CloseRequested => {
                if self.app.close_requested() {
                    event_loop.exit();
                }
            }
            WindowEvent::SurfaceResized(_) | WindowEvent::ScaleFactorChanged { .. } => {
                self.app.resized(window);
            }
            WindowEvent::Occluded(occluded) => self.app.occluded(window, occluded),
            WindowEvent::RedrawRequested => self.app.redraw(window),
            WindowEvent::Focused(gained) => self.app.event(window, &Event::Focus(gained)),
            WindowEvent::ModifiersChanged(m) => {
                let s = m.state();
                self.mods = Mods {
                    shift: s.shift_key(),
                    ctrl: s.control_key(),
                    alt: s.alt_key(),
                    meta: s.meta_key(),
                };
            }
            WindowEvent::PointerMoved { position, .. } => {
                let (x, y) = logical(position.x, position.y);
                self.pointer = (x, y);
                self.app.event(window, &Event::PointerMove { x, y });
            }
            WindowEvent::PointerButton {
                state,
                position,
                button,
                ..
            } => {
                let (x, y) = logical(position.x, position.y);
                self.pointer = (x, y);
                let button = match button {
                    winit::event::ButtonSource::Mouse(b) => match b {
                        winit::event::MouseButton::Left => Button::Primary,
                        winit::event::MouseButton::Right => Button::Secondary,
                        winit::event::MouseButton::Middle => Button::Middle,
                        other => Button::Other(other as u16),
                    },
                    _ => Button::Primary,
                };
                let ev = match state {
                    ElementState::Pressed => Event::PointerDown {
                        x,
                        y,
                        button,
                        mods: self.mods,
                    },
                    ElementState::Released => Event::PointerUp { x, y, button },
                };
                self.app.event(window, &ev);
            }
            WindowEvent::PointerLeft { position, .. } => {
                // The pointer left the surface; report a move outside so
                // hover/leave synthesis runs.
                if let Some(p) = position {
                    let (x, y) = logical(p.x, p.y);
                    self.pointer = (x, y);
                    self.app.event(window, &Event::PointerMove { x, y });
                }
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let (dx, dy) = match delta {
                    // Pixel deltas are physical px -> logical points.
                    MouseScrollDelta::PixelDelta(p) => (
                        (p.x as f32) / scale,
                        (p.y as f32) / scale,
                    ),
                    // Line deltas: ~32pt per line, a common convention.
                    MouseScrollDelta::LineDelta(x, y) => (x * 32.0, y * 32.0),
                    _ => return,
                };
                // Winit deltas are positive when the content moves
                // down/right (revealing earlier content); Craie offsets
                // grow downward/rightward, so flip the sign.
                let (x, y) = self.pointer;
                self.app.event(
                    window,
                    &Event::Wheel {
                        x,
                        y,
                        dx: -dx,
                        dy: -dy,
                    },
                );
            }
            WindowEvent::KeyboardInput { event, .. } => {
                let input = self.key_input(&event);
                let ev = match event.state {
                    ElementState::Pressed => Event::KeyDown(input),
                    ElementState::Released => Event::KeyUp(input),
                };
                self.app.event(window, &ev);
            }
            WindowEvent::Ime(ime) => match ime {
                Ime::Enabled => {}
                Ime::Preedit(text, cursor) => {
                    self.app.event(
                        window,
                        &Event::ImePreedit {
                            text,
                            cursor,
                        },
                    );
                }
                Ime::Commit(text) => self.app.event(window, &Event::ImeCommit(text)),
                Ime::Disabled => self.app.event(window, &Event::ImeDone),
                _ => {}
            },
            _ => {}
        }
    }
}
