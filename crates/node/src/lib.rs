//! N-API runtime: `NativeHost` owns the window + event loop on the main
//! thread; `NativeClient` runs on the JS worker and submits encoded wire
//! transactions into the shared `Session`.
//!
//! Modeled on gpui-react's runtime: `runApplication` on the main thread
//! constructs `NativeHost`, spawns the app in a `worker_thread`, and calls
//! `run()`; the worker constructs `NativeClient` with the session id and
//! calls `submit`/`receive`/`close`.

use std::cell::Cell;
use std::sync::{Arc, Mutex, OnceLock};

use craie::app::HostApp;
use craie::bridge::{Session, Sessions};
use craie::custom::{CustomData, Painter, Quad};
use craie::geom::{Rect, Size};
use napi::bindgen_prelude::{FunctionRef, Uint8Array};
use napi::threadsafe_function::{ThreadsafeFunction, ThreadsafeFunctionCallMode};
use napi::{Env, Error, Result, Status};
use napi_derive::napi;

/// UI -> JS callback channel: `CalleeHandled` off (the callback receives
/// the frame directly, not `(err, value)`), strong (an attached client
/// keeps the worker's event loop alive; `Session::close` clears the
/// notify callback, which drops and releases the function), and a bounded
/// queue so a stalled JS side cannot grow memory without limit.
type OutFn = ThreadsafeFunction<Uint8Array, (), Uint8Array, Status, false, false, 1024>;

static SESSIONS: OnceLock<Sessions> = OnceLock::new();

fn sessions() -> &'static Sessions {
    SESSIONS.get_or_init(Sessions::new)
}

/// The thread that owns the platform event loop. On macOS this is
/// genuinely the main thread (AppKit requires it); elsewhere there is no
/// portable main-thread query, so the first `NativeHost` creation declares
/// it and winit enforces the real contract when the loop is created.
static MAIN_THREAD: OnceLock<std::thread::ThreadId> = OnceLock::new();

fn is_main_thread() -> bool {
    #[cfg(target_os = "macos")]
    unsafe {
        return libc::pthread_main_np() != 0;
    }
    #[cfg(not(target_os = "macos"))]
    {
        match MAIN_THREAD.get() {
            Some(id) => *id == std::thread::current().id(),
            // No host yet: allow this thread to claim main-thread status.
            None => true,
        }
    }
}

fn declare_main_thread() {
    MAIN_THREAD.get_or_init(|| std::thread::current().id());
}

thread_local! {
    static RUNNING: Cell<bool> = const { Cell::new(false) };
}

#[napi(object)]
pub struct HostOptions {
    pub title: Option<String>,
    pub width: Option<f64>,
    pub height: Option<f64>,
}

/// The argument a registered painter receives for each custom node:
/// the wire payload plus the node's logical content rect.
#[napi(object)]
pub struct PaintSpec {
    pub tag: u32,
    pub data: Vec<f64>,
    pub text: String,
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

/// One filled rect a painter emits, in logical points relative to the
/// window (same space `PaintSpec` reports). Maps to `Instance::rect`.
#[napi(object)]
pub struct PaintQuad {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
    /// Fill, 0xRRGGBBAA.
    pub color: u32,
    pub radius: Option<f64>,
    pub border_w: Option<f64>,
    pub border_color: Option<u32>,
}

/// Wraps a JS paint function into the native `Painter` signature. Runs
/// on the UI thread during paint — a throwing or missing callback paints
/// nothing for that node.
fn js_painter(env: Env, fref: FunctionRef<PaintSpec, Vec<PaintQuad>>) -> Painter {
    Box::new(move |data: &CustomData, rect: Rect, out: &mut Vec<Quad>| {
        let Ok(func) = fref.borrow_back(&env) else { return };
        let spec = PaintSpec {
            tag: data.tag,
            data: data.data.iter().map(|&v| v as f64).collect(),
            text: data.text.clone(),
            x: rect.origin.x as f64,
            y: rect.origin.y as f64,
            w: rect.size.width as f64,
            h: rect.size.height as f64,
        };
        match func.call(spec) {
            Ok(quads) => out.extend(quads.into_iter().map(|q| Quad {
                x: q.x as f32,
                y: q.y as f32,
                w: q.w as f32,
                h: q.h as f32,
                color: q.color,
                radius: q.radius.unwrap_or(0.0) as f32,
                border_w: q.border_w.unwrap_or(0.0) as f32,
                border_color: q.border_color.unwrap_or(0),
            })),
            Err(e) => eprintln!("[craie-node] painter {} failed: {e}", data.tag),
        }
    })
}

#[napi]
pub struct NativeHost {
    id: u32,
    session: Arc<Session>,
    title: String,
    width: f64,
    height: f64,
    /// JS painters registered before `run`; drained into the `Ui` when
    /// the event loop starts. Main-thread only.
    painters: Mutex<Vec<(u32, Env, FunctionRef<PaintSpec, Vec<PaintQuad>>)>>,
}

#[napi]
impl NativeHost {
    #[napi(constructor)]
    pub fn new(options: Option<HostOptions>) -> Result<Self> {
        if !is_main_thread() {
            return Err(Error::from_reason("NativeHost requires the main thread"));
        }
        declare_main_thread();
        if RUNNING.with(|r| r.get()) {
            return Err(Error::from_reason("A native host is already active"));
        }
        let options = options.unwrap_or(HostOptions {
            title: None,
            width: None,
            height: None,
        });
        let width = options.width.unwrap_or(800.0) as f32;
        let height = options.height.unwrap_or(600.0) as f32;
        if !width.is_finite() || !height.is_finite() || width <= 0.0 || height <= 0.0 {
            return Err(Error::from_reason(
                "Window dimensions must be finite and positive",
            ));
        }
        let session = Session::new();
        let id = sessions().insert(&session);
        Ok(Self {
            id,
            session,
            title: options.title.unwrap_or_else(|| "Craie".into()),
            width: width as f64,
            height: height as f64,
            painters: Mutex::new(Vec::new()),
        })
    }

    /// The session id a worker passes to `NativeClient`.
    #[napi(getter)]
    pub fn id(&self) -> u32 {
        self.id
    }

    /// Registers a JS painter for `<Custom>` elements with payload `tag`.
    /// Called on the UI thread during paint with a `PaintSpec`; returns
    /// an array of `PaintQuad`s in logical points. Must be called before
    /// `run`.
    #[napi]
    pub fn register_painter(
        &self,
        env: Env,
        tag: u32,
        callback: FunctionRef<PaintSpec, Vec<PaintQuad>>,
    ) {
        self.painters.lock().unwrap().push((tag, env, callback));
    }

    /// Runs the platform event loop on this thread until the window
    /// closes or the session ends. Returns the close reason.
    #[napi]
    pub fn run(&self) -> Result<String> {
        RUNNING.with(|r| r.set(true));
        let session = self.session.clone();
        let mut app = HostApp::new(session.clone());
        for (tag, env, fref) in self.painters.lock().unwrap().drain(..) {
            app.register_painter(tag, js_painter(env, fref));
        }
        craie::platform::run(
            &self.title,
            Size::new(self.width as f32, self.height as f32),
            app,
        );
        RUNNING.with(|r| r.set(false));
        sessions().remove(self.id);
        let reason = session
            .closed_reason()
            .unwrap_or_else(|| "Native window closed".into());
        session.close(reason.clone());
        Ok(reason)
    }

    #[napi]
    pub fn close(&self, reason: String) {
        self.session.close(reason);
    }
}

impl Drop for NativeHost {
    fn drop(&mut self) {
        self.session.close("Native host disposed");
        sessions().remove(self.id);
    }
}

#[napi]
pub struct NativeClient {
    session: Arc<Session>,
}

#[napi]
impl NativeClient {
    #[napi(constructor)]
    pub fn new(id: u32, env: Env) -> Result<Self> {
        if is_main_thread() {
            return Err(Error::from_reason(
                "NativeClient must run on the application worker",
            ));
        }
        let session = sessions()
            .get(id)
            .ok_or_else(|| Error::from_reason("Unknown native session"))?;
        if session.is_closed() {
            return Err(Error::from_reason("Native session is closed"));
        }
        env.add_env_cleanup_hook(session.clone(), |session| {
            session.close("Application worker unloaded")
        })?;
        Ok(Self { session })
    }

    /// Queues one encoded transaction for the UI thread. The bytes are
    /// copied once — one copy per React commit.
    #[napi]
    pub fn submit(&self, bytes: Uint8Array) -> Result<()> {
        self.session.submit(bytes.to_vec()).map_err(Error::from_reason)
    }

    /// Subscribes to UI -> JS output: `callback(frame)` fires on this
    /// worker's event loop with a tagged binary frame — tag 0 is an ack
    /// batch (`u32 count` + `u64 seq`s), tag 1 an event batch (see
    /// `events::encode_events`).
    #[napi]
    pub fn subscribe(&self, callback: OutFn) -> Result<()> {
        let tsfn = Arc::new(callback);
        let session = self.session.clone();
        let weak = Arc::downgrade(&session);
        let pump_tsfn = tsfn.clone();
        session.set_out_notify(move || {
            let Some(s) = weak.upgrade() else { return };
            for frame in s.take_out() {
                pump_tsfn.call(frame.into(), ThreadsafeFunctionCallMode::NonBlocking);
            }
        });
        // Drain anything queued before the subscription landed.
        for frame in session.take_out() {
            tsfn.call(frame.into(), ThreadsafeFunctionCallMode::NonBlocking);
        }
        Ok(())
    }

    #[napi]
    pub fn close(&self, reason: String) {
        self.session.close(reason);
    }
}

#[napi]
pub fn craie_runtime_version() -> u32 {
    1
}
