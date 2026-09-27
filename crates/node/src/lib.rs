//! N-API runtime: `NativeHost` owns the window + event loop on the main
//! thread; `NativeClient` runs on the JS worker and submits encoded wire
//! transactions into the shared `Session`.
//!
//! Modeled on gpui-react's runtime: `runApplication` on the main thread
//! constructs `NativeHost`, spawns the app in a `worker_thread`, and calls
//! `run()`; the worker constructs `NativeClient` with the session id and
//! calls `submit`/`receive`/`close`.

use std::cell::Cell;
use std::sync::{Arc, OnceLock};

use craie_core::geom::Size;
use craie_platform_winit::app::HostApp;
use craie_ui::bridge::{Delivery, Session, Sessions};
use napi::bindgen_prelude::Uint8Array;
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
        libc::pthread_main_np() != 0
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

#[napi]
pub struct NativeHost {
    id: u32,
    session: Arc<Session>,
    title: String,
    width: f64,
    height: f64,
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
        })
    }

    /// The session id a worker passes to `NativeClient`.
    #[napi(getter)]
    pub fn id(&self) -> u32 {
        self.id
    }

    /// Runs the platform event loop on this thread until the window
    /// closes or the session ends. Returns the close reason.
    #[napi]
    pub fn run(&self) -> Result<String> {
        RUNNING.with(|r| r.set(true));
        let session = self.session.clone();
        let size = Size::new(self.width as f32, self.height as f32);
        // `CRAIE_HEADLESS`: no window (measurements with no display on).
        if std::env::var_os("CRAIE_HEADLESS").is_some() {
            craie_platform_winit::headless::run(session.clone(), size, 2.0);
        } else {
            let app = HostApp::new(session.clone());
            craie_platform_winit::run(&self.title, size, app);
        }
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
        self.session
            .submit(bytes.to_vec())
            .map_err(Error::from_reason)
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
        // A frame the bounded queue refuses waits in the session and goes
        // out, in order, on the next pump (`resume` runs one after each
        // delivered frame); a closed queue closes the session.
        session.set_out_notify(move || {
            let Some(s) = weak.upgrade() else { return };
            s.pump(|frame| {
                match tsfn.call(frame.into(), ThreadsafeFunctionCallMode::NonBlocking) {
                    Status::Ok => Delivery::Sent,
                    Status::QueueFull => Delivery::Full,
                    _ => Delivery::Closed,
                }
            });
        });
        // Deliver anything queued before the subscription landed.
        session.poke_out();
        Ok(())
    }

    /// The JS side took a frame: frames the full queue refused go out
    /// now (`Session::resume`: always a pump, ordered after one that is
    /// storing a refused frame).
    #[napi]
    pub fn resume(&self) {
        self.session.resume();
    }

    #[napi]
    pub fn close(&self, reason: String) {
        self.session.close(reason);
    }
}

#[napi]
/// Bridge protocol version, the wire's own (`wire::VERSION`), so the
/// load-time check and the transaction header can't disagree. 5 = 4
/// (CRW2 transactions with claims, generation-stamped 36-byte event
/// records, key records with modifiers and the physical key) plus
/// inherited color: 45-byte drawing shapes with a `current` byte, and
/// INPUT_CONFIG without a color.
pub fn craie_runtime_version() -> u32 {
    u32::from(craie_ui::wire::VERSION)
}
