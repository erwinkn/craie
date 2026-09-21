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

use craie::app::HostApp;
use craie::bridge::{Session, Sessions};
use craie::geom::Size;
use napi::bindgen_prelude::{AsyncTask, Uint8Array};
use napi::{Env, Error, Result};
use napi_derive::napi;

static SESSIONS: OnceLock<Sessions> = OnceLock::new();

fn sessions() -> &'static Sessions {
    SESSIONS.get_or_init(Sessions::new)
}

fn is_main_thread() -> bool {
    #[cfg(target_os = "macos")]
    unsafe {
        libc::pthread_main_np() != 0
    }
    #[cfg(not(target_os = "macos"))]
    {
        true // permissive until a non-macOS check is needed
    }
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
        craie::platform::run(
            &self.title,
            Size::new(self.width as f32, self.height as f32),
            HostApp::new(session.clone()),
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

    /// Resolves with the seqs applied since the last call; rejects (via
    /// empty batch semantics) once the session closes. Only one
    /// outstanding `receive` at a time — the JS side loops on it.
    #[napi]
    pub fn receive(&self) -> AsyncTask<Receive> {
        AsyncTask::new(Receive(self.session.clone()))
    }

    #[napi]
    pub fn close(&self, reason: String) {
        self.session.close(reason);
    }
}

pub struct Receive(Arc<Session>);

impl napi::Task for Receive {
    type Output = Vec<f64>;
    type JsValue = Vec<f64>;

    fn compute(&mut self) -> Result<Self::Output> {
        Ok(self.0.recv_acks().iter().map(|s| *s as f64).collect())
    }

    fn resolve(&mut self, _: Env, output: Self::Output) -> Result<Self::JsValue> {
        Ok(output)
    }
}

#[napi]
pub fn craie_runtime_version() -> u32 {
    1
}
