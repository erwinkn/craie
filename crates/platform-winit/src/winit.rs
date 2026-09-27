//! Winit driver: owns the event loop, delivers Craie-level callbacks.
//!
//! Control flow is `Wait`: the loop sleeps until the OS or the app requests
//! work, or until the app's next timer (`App::next_timer`, e.g. settling
//! scroll content). `RedrawRequested` is the only place frames are
//! produced.
//!
//! Input normalization lives here: winit window events become
//! `events::Event`s in logical points, so nothing above this module names
//! a winit type. Modifier state is tracked from `ModifiersChanged`.

use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
use winit::event::{ElementState, Ime, MouseButton, MouseScrollDelta, StartCause, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{Key as WKey, KeyCode, NamedKey, PhysicalKey};
use winit::window::{WindowAttributes, WindowId};

use craie_core::Size;
use craie_ui::events::{Button, Event, Key, KeyInput, Mods};

use crate::{App, Wake, Window};

pub fn run<A: App>(title: &str, logical_size: Size, app: A) {
    let event_loop = EventLoop::<()>::with_user_event()
        .build()
        .expect("failed to create event loop");
    event_loop.set_control_flow(ControlFlow::Wait);
    let mut driver = Driver {
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
            .with_inner_size(LogicalSize::new(
                logical_size.width as f64,
                logical_size.height as f64,
            )),
        mods: Mods::default(),
        pointer: (0.0, 0.0),
        dropped: Vec::new(),
        ctrl_click: false,
    };
    event_loop.run_app(&mut driver).expect("event loop error");
}

struct Driver<A: App> {
    app: A,
    window: Option<Window>,
    wake: Wake,
    attrs: WindowAttributes,
    mods: Mods,
    /// Last pointer position (logical); wheel events carry no position.
    pointer: (f32, f32),
    /// Files dropped since the last wait: winit sends one event per
    /// file, the app gets one drop.
    dropped: Vec<String>,
    /// The held primary press began with Ctrl on macOS: it is a
    /// secondary press, and so is its release.
    ctrl_click: bool,
}

impl<A: App> Driver<A> {
    fn key_input(&self, event: &winit::event::KeyEvent) -> KeyInput {
        let key = match &event.logical_key {
            WKey::Named(named) => named_key(named),
            _ => Key::Unknown,
        };
        // Shift and Alt apply, Ctrl does not (the web's `event.key`,
        // which chords are written against); `code` backs it up.
        let char = match &event.logical_key {
            WKey::Character(c) => Some(c.to_string()),
            _ => None,
        };
        let code = match event.physical_key {
            PhysicalKey::Code(code) => us_char(code),
            PhysicalKey::Unidentified(_) => None,
        };
        // `text` is the printable string for this press. Command chords
        // (meta/ctrl) are shortcuts, not text — the platform filters them
        // out here rather than at the editor.
        let text = if self.mods.meta || self.mods.ctrl {
            None
        } else {
            event
                .text
                .as_ref()
                .map(|t| t.to_string())
                .or_else(|| (key == Key::Unknown).then(|| char.clone()).flatten())
        };
        KeyInput {
            key,
            text,
            char,
            code,
            mods: self.mods,
            repeat: event.repeat,
        }
    }
}

fn named_key(named: &NamedKey) -> Key {
    match named {
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
        NamedKey::Space => Key::Space,
        NamedKey::Insert => Key::Insert,
        NamedKey::ContextMenu => Key::ContextMenu,
        _ => F_KEYS
            .iter()
            .position(|f| f == named)
            .map_or(Key::Unknown, |i| Key::F(i as u8 + 1)),
    }
}

const F_KEYS: [NamedKey; 24] = [
    NamedKey::F1,
    NamedKey::F2,
    NamedKey::F3,
    NamedKey::F4,
    NamedKey::F5,
    NamedKey::F6,
    NamedKey::F7,
    NamedKey::F8,
    NamedKey::F9,
    NamedKey::F10,
    NamedKey::F11,
    NamedKey::F12,
    NamedKey::F13,
    NamedKey::F14,
    NamedKey::F15,
    NamedKey::F16,
    NamedKey::F17,
    NamedKey::F18,
    NamedKey::F19,
    NamedKey::F20,
    NamedKey::F21,
    NamedKey::F22,
    NamedKey::F23,
    NamedKey::F24,
];

/// A physical key as the character it types on a US layout: letters,
/// digits and punctuation (`KeyInput::code`).
fn us_char(code: KeyCode) -> Option<char> {
    const LETTERS: [KeyCode; 26] = [
        KeyCode::KeyA,
        KeyCode::KeyB,
        KeyCode::KeyC,
        KeyCode::KeyD,
        KeyCode::KeyE,
        KeyCode::KeyF,
        KeyCode::KeyG,
        KeyCode::KeyH,
        KeyCode::KeyI,
        KeyCode::KeyJ,
        KeyCode::KeyK,
        KeyCode::KeyL,
        KeyCode::KeyM,
        KeyCode::KeyN,
        KeyCode::KeyO,
        KeyCode::KeyP,
        KeyCode::KeyQ,
        KeyCode::KeyR,
        KeyCode::KeyS,
        KeyCode::KeyT,
        KeyCode::KeyU,
        KeyCode::KeyV,
        KeyCode::KeyW,
        KeyCode::KeyX,
        KeyCode::KeyY,
        KeyCode::KeyZ,
    ];
    const DIGITS: [KeyCode; 10] = [
        KeyCode::Digit0,
        KeyCode::Digit1,
        KeyCode::Digit2,
        KeyCode::Digit3,
        KeyCode::Digit4,
        KeyCode::Digit5,
        KeyCode::Digit6,
        KeyCode::Digit7,
        KeyCode::Digit8,
        KeyCode::Digit9,
    ];
    if let Some(i) = LETTERS.iter().position(|&k| k == code) {
        return Some((b'a' + i as u8) as char);
    }
    if let Some(i) = DIGITS.iter().position(|&k| k == code) {
        return Some((b'0' + i as u8) as char);
    }
    Some(match code {
        KeyCode::Minus => '-',
        KeyCode::Equal => '=',
        KeyCode::BracketLeft => '[',
        KeyCode::BracketRight => ']',
        KeyCode::Backslash => '\\',
        KeyCode::Semicolon => ';',
        KeyCode::Quote => '\'',
        KeyCode::Backquote => '`',
        KeyCode::Comma => ',',
        KeyCode::Period => '.',
        KeyCode::Slash => '/',
        _ => return None,
    })
}

impl<A: App> ApplicationHandler for Driver<A> {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        let window = event_loop
            .create_window(self.attrs.clone())
            .expect("failed to create window");
        let a11y = self.app.a11y_shared().map(|shared| {
            accesskit_winit::Adapter::with_direct_handlers(
                event_loop,
                &window,
                craie_ui::a11y::Activation {
                    shared: shared.clone(),
                },
                crate::a11y::ActionSink {
                    shared,
                    wake: self.wake.clone(),
                },
                craie_ui::a11y::Deactivation,
            )
        });
        let window = Window::new(window, a11y);
        // Adapter in place: safe to show.
        window.inner.set_visible(true);
        self.app.ready(&window, &self.wake);
        window.request_redraw();
        self.window = Some(window);
    }

    fn new_events(&mut self, _event_loop: &ActiveEventLoop, cause: StartCause) {
        if let StartCause::ResumeTimeReached { .. } = cause
            && let Some(window) = &self.window
        {
            self.app.timer(window);
        }
    }

    /// Delivers the files dropped this turn as one drop, then sleeps
    /// until the app's next timer, if it has one.
    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        if !self.dropped.is_empty()
            && let Some(window) = &self.window
        {
            let (x, y) = self.pointer;
            let paths = std::mem::take(&mut self.dropped);
            self.app.event(window, &Event::Drop { x, y, paths });
        }
        event_loop.set_control_flow(match self.app.next_timer() {
            Some(at) => ControlFlow::WaitUntil(at),
            None => ControlFlow::Wait,
        });
    }

    /// A `Wake::wake` arrived from another thread. The payload is implicit:
    /// the app drains whatever queues it owns.
    fn user_event(&mut self, event_loop: &ActiveEventLoop, (): ()) {
        if let Some(window) = &self.window
            && self.app.woke(window)
        {
            event_loop.exit();
        }
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
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
            WindowEvent::Resized(_) | WindowEvent::ScaleFactorChanged { .. } => {
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
                    meta: s.super_key(),
                };
            }
            WindowEvent::CursorMoved { position, .. } => {
                let (x, y) = logical(position.x, position.y);
                self.pointer = (x, y);
                self.app.event(window, &Event::PointerMove { x, y });
            }
            WindowEvent::MouseInput { state, button, .. } => {
                // Buttons carry no position in winit 0.30; the last
                // cursor position is where the press happened.
                let (x, y) = self.pointer;
                let mut button = match button {
                    MouseButton::Left => Button::Primary,
                    MouseButton::Right => Button::Secondary,
                    MouseButton::Middle => Button::Middle,
                    MouseButton::Back => Button::Other(4),
                    MouseButton::Forward => Button::Other(5),
                    MouseButton::Other(b) => Button::Other(b),
                };
                // macOS: Ctrl+click is a secondary click (context menus).
                if cfg!(target_os = "macos") && button == Button::Primary {
                    if state == ElementState::Pressed {
                        self.ctrl_click = self.mods.ctrl;
                    }
                    if self.ctrl_click {
                        button = Button::Secondary;
                    }
                    if state == ElementState::Released {
                        self.ctrl_click = false;
                    }
                }
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
            WindowEvent::CursorLeft { .. } => {
                // The pointer left the surface; report a move outside so
                // hover/leave synthesis runs.
                self.pointer = (-1.0, -1.0);
                self.app
                    .event(window, &Event::PointerMove { x: -1.0, y: -1.0 });
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let (dx, dy) = match delta {
                    // Pixel deltas are physical px -> logical points.
                    MouseScrollDelta::PixelDelta(p) => ((p.x as f32) / scale, (p.y as f32) / scale),
                    // Line deltas: ~32pt per line, a common convention.
                    MouseScrollDelta::LineDelta(x, y) => (x * 32.0, y * 32.0),
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
            // Drops carry no position in winit 0.30, and no cursor moves
            // arrive while a drag from another app is over the window:
            // the position is unknown until the pointer moves again, and
            // the drop goes to the focus path (`LEDGER.md` DF-12).
            WindowEvent::HoveredFile(_) => self.pointer = (-1.0, -1.0),
            WindowEvent::DroppedFile(path) => {
                self.dropped.push(path.to_string_lossy().into_owned());
            }
            WindowEvent::Ime(ime) => match ime {
                Ime::Enabled => {}
                Ime::Preedit(text, cursor) => {
                    self.app.event(window, &Event::ImePreedit { text, cursor });
                }
                Ime::Commit(text) => self.app.event(window, &Event::ImeCommit(text)),
                Ime::Disabled => self.app.event(window, &Event::ImeDone),
            },
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn named_keys() {
        assert_eq!(named_key(&NamedKey::F1), Key::F(1));
        assert_eq!(named_key(&NamedKey::F13), Key::F(13));
        assert_eq!(named_key(&NamedKey::F24), Key::F(24));
        assert_eq!(named_key(&NamedKey::F25), Key::Unknown);
        assert_eq!(named_key(&NamedKey::ContextMenu), Key::ContextMenu);
        assert_eq!(named_key(&NamedKey::Shift), Key::Unknown);
    }

    #[test]
    fn us_chars() {
        let cases = [
            (KeyCode::KeyA, Some('a')),
            (KeyCode::KeyZ, Some('z')),
            (KeyCode::Digit0, Some('0')),
            (KeyCode::Digit9, Some('9')),
            (KeyCode::Slash, Some('/')),
            (KeyCode::Backquote, Some('`')),
            (KeyCode::Quote, Some('\'')),
            (KeyCode::Space, None),
            (KeyCode::Numpad1, None),
            (KeyCode::IntlBackslash, None),
        ];
        for (code, char) in cases {
            assert_eq!(us_char(code), char, "{code:?}");
        }
    }
}
