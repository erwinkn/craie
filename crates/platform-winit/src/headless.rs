//! `CRAIE_HEADLESS=1`: runs a session with no window, for measuring on
//! a machine whose display is off or locked (macOS throttles the frame
//! loop of a window it cannot show). The same `Ui`, frame path
//! (`prepare_frame`), renderer (into an offscreen target, at the window
//! format), and frame statistics as `HostApp`; frames are paced at 120
//! Hz while the UI owes a paint or animates (spinning between frames:
//! the pacing must not depend on coalesced timers), and each frame
//! waits for the GPU. There is no input: the app drives itself, or the
//! E19 probe clicks (`probe.rs`).

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use craie_core::Size;
use craie_render::{Gpu, Renderer};
use craie_ui::bridge::Session;
use craie_ui::events;
use craie_ui::ui::Ui;

use crate::app::{FrameStats, prepare_frame};
use crate::probe::Probe;

const FRAME: Duration = Duration::from_nanos(8_333_333);
/// Waits shorter than this spin (see the wait below).
const SPIN: Duration = Duration::from_millis(20);
/// With the E19 probe on, every wait spins, on macOS: there a process
/// that a background agent started had its timed waits wake up to 150 ms
/// late (coalesced timers), which E19 measured as clicks waiting for
/// native. Linux timers were not late, so its waits keep `SPIN`.
const PROBE_SPINS: bool = cfg!(target_vendor = "apple");
const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Bgra8UnormSrgb;

/// Runs `session` headless at `logical` size and display `scale` until
/// the session closes. Returns the close reason.
pub fn run(session: Arc<Session>, logical: Size, scale: f32) -> String {
    run_counted(session, logical, scale, &AtomicU64::new(0))
}

/// `run`, counting drawn frames in `frames` (tests).
fn run_counted(session: Arc<Session>, logical: Size, scale: f32, frames: &AtomicU64) -> String {
    interactive_thread();
    crate::fonts::install();
    let Some(gpu) = Gpu::try_headless() else {
        session.close("no GPU adapter");
        return "no GPU adapter".into();
    };
    let mut renderer = Renderer::new(&gpu, FORMAT);
    let (w, h) = (
        (logical.width * scale).round() as u32,
        (logical.height * scale).round() as u32,
    );
    let target = gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("headless"),
        size: wgpu::Extent3d {
            width: w,
            height: h,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: FORMAT,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        view_formats: &[],
    });
    let view = target.create_view(&Default::default());
    let start = Instant::now();
    let mut ui = Ui::new(scale);
    ui.set_window_size(logical);
    ui.set_time(0.0);

    // The flag is atomic so a spin can poll it without the lock, which
    // is only for the condvar: a spinner holding it could keep a waker
    // (the JS thread) waiting.
    let woken = Arc::new((AtomicBool::new(false), Mutex::new(()), Condvar::new()));
    let signal = woken.clone();
    let wake = move || {
        signal.0.store(true, Ordering::Release);
        let _lock = signal.1.lock().unwrap();
        signal.2.notify_one();
    };
    let mut images = crate::images::Decoder::new(wake.clone());
    session.install_wake(Arc::new(wake));

    let mut stats = FrameStats::new();
    let mut spin = SpinLog::new();
    let mut probe = Probe::from_env();
    let mut next_frame = Instant::now();
    loop {
        if session.is_closed() {
            break;
        }
        ui.set_time(start.elapsed().as_secs_f64());
        let t = Instant::now();
        for buf in session.take_commits() {
            match ui.apply(&buf) {
                Ok(seq) => session.ack(seq),
                Err(e) => {
                    eprintln!("[craie] undecodable transaction: {e:?}");
                    session.close("undecodable transaction");
                }
            }
        }
        stats.worked(t.elapsed().as_secs_f64() * 1e3);
        images.pump(&mut ui);
        if ui
            .next_settle()
            .is_some_and(|at| at <= start.elapsed().as_secs_f64())
        {
            ui.settle();
        }
        if let Some(p) = &mut probe {
            p.applied(&ui);
            for e in &p.clicks() {
                ui.dispatch(e);
            }
            p.finish(&session);
        }
        // Commits raise events with no paint (an animate that ends at
        // once): they go out now, not with the next frame.
        flush_events(&mut ui, &session);
        let owes = ui.needs_paint() || ui.animating();
        if owes && Instant::now() >= next_frame {
            next_frame = Instant::now() + FRAME;
            ui.set_time(start.elapsed().as_secs_f64());
            let t = Instant::now();
            prepare_frame(&mut ui, &mut renderer, &gpu, (w, h), scale);
            let prepare_ms = t.elapsed().as_secs_f64() * 1e3;
            images.pump(&mut ui);
            flush_events(&mut ui, &session);
            let t = Instant::now();
            renderer.draw(&gpu, &view, w, h, ui.scene_mut());
            let cpu_ms = prepare_ms + t.elapsed().as_secs_f64() * 1e3;
            let _ = gpu.device.poll(wgpu::PollType::Wait {
                submission_index: None,
                timeout: None,
            });
            frames.fetch_add(1, Ordering::SeqCst);
            if let Some(p) = &mut probe {
                p.presented(&ui);
                p.finish(&session);
            }
            let tweens = ui.animation_count();
            if let Some(e) = stats.frame(cpu_ms, prepare_ms, ui.host.len(), tweens) {
                session.post_events(events::encode_events(&[e]), false);
            }
        }
        // Sleep until the next frame (owed), the next settle, or a wake.
        let owes = ui.needs_paint() || ui.animating();
        let settle = ui
            .next_settle()
            .map(|at| start + Duration::from_secs_f64(at.max(0.0)));
        let until = [
            owes.then_some(next_frame),
            settle,
            probe.as_ref().and_then(Probe::due),
        ]
        .into_iter()
        .flatten()
        .min();
        let (flag, lock, cv) = &*woken;
        while !flag.load(Ordering::Acquire) {
            match until {
                // Near deadlines spin: this process's timed waits wake
                // up to 30 ms late (coalesced timers, no visible window),
                // which would pace frames by the timer, not the work.
                Some(at)
                    if (PROBE_SPINS && probe.is_some())
                        || at.saturating_duration_since(Instant::now()) < SPIN =>
                {
                    let spun = thread_cpu();
                    while Instant::now() < at && !flag.load(Ordering::Acquire) {
                        std::thread::yield_now();
                    }
                    spin.spun(thread_cpu().saturating_sub(spun));
                    break;
                }
                // Wakers set the flag before they take the lock, so it is
                // checked under the lock: a wake cannot slip in between.
                Some(at) => {
                    let wait = at
                        .saturating_duration_since(Instant::now())
                        .saturating_sub(SPIN);
                    let held = lock.lock().unwrap();
                    if !flag.load(Ordering::Acquire) {
                        drop(cv.wait_timeout(held, wait).unwrap());
                    }
                }
                None => {
                    let held = lock.lock().unwrap();
                    if !flag.load(Ordering::Acquire) {
                        drop(cv.wait(held).unwrap());
                    }
                }
            }
        }
        flag.store(false, Ordering::Release);
    }
    session
        .closed_reason()
        .unwrap_or_else(|| "headless session closed".into())
}

/// `CRAIE_HEADLESS_LOG`: the CPU time this thread spends spinning
/// between frames (its thread CPU clock, not elapsed time), as running
/// totals on stderr each second, so a measurement can take it out of
/// process CPU over the same interval.
struct SpinLog {
    on: bool,
    last: Instant,
    spun: Duration,
}

impl SpinLog {
    fn new() -> SpinLog {
        SpinLog {
            on: std::env::var_os("CRAIE_HEADLESS_LOG").is_some(),
            last: Instant::now(),
            spun: Duration::ZERO,
        }
    }

    fn spun(&mut self, cpu: Duration) {
        self.spun += cpu;
        if self.on && self.last.elapsed() >= Duration::from_secs(1) {
            self.last = Instant::now();
            let epoch = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default();
            eprintln!(
                "[headless] epoch {}: spin cpu {:.1} ms in total",
                epoch.as_millis(),
                self.spun.as_secs_f64() * 1e3
            );
        }
    }
}

/// This thread's CPU time (zero where no thread CPU clock is wired:
/// the spin log then reports zero).
#[cfg(not(unix))]
fn thread_cpu() -> Duration {
    Duration::ZERO
}

/// This thread's CPU time.
#[cfg(unix)]
fn thread_cpu() -> Duration {
    let mut t = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: clock_gettime writes the timespec it is given.
    unsafe { libc::clock_gettime(libc::CLOCK_THREAD_CPUTIME_ID, &mut t) };
    Duration::new(t.tv_sec as u64, t.tv_nsec as u32)
}

/// Sends the UI's queued events to JS.
fn flush_events(ui: &mut Ui, session: &Session) {
    let out = ui.take_events();
    if !out.is_empty() {
        // Animation ends resolve JS promises, an image loads or fails
        // once, and an exit's end frees JS's parked ids: those frames
        // never drop.
        let reliable = out.iter().any(|e| {
            matches!(
                e.kind,
                events::out_kind::ANIMATION_END
                    | events::out_kind::IMAGE
                    | events::out_kind::EXIT_END
            )
        });
        session.post_events(events::encode_events(&out), reliable);
    }
}

/// Asks macOS for user-interactive timer precision on this thread: a
/// process with no visible window otherwise gets coalesced timers, and
/// a 4 ms wait woke up to 31 ms late.
#[cfg(target_os = "macos")]
fn interactive_thread() {
    unsafe extern "C" {
        fn pthread_set_qos_class_self_np(qos_class: u32, relative_priority: i32) -> i32;
    }
    const QOS_CLASS_USER_INTERACTIVE: u32 = 0x21;
    // SAFETY: sets the calling thread's QoS class; no pointers involved.
    unsafe {
        pthread_set_qos_class_self_np(QOS_CLASS_USER_INTERACTIVE, 0);
    }
}

#[cfg(not(target_os = "macos"))]
fn interactive_thread() {}

#[cfg(test)]
mod tests {
    use super::*;
    use craie_ui::animation::{Prop, Timing, Value};
    use craie_ui::bridge::Delivery;
    use craie_ui::mutation::{NIL, NodeKind, Transaction};

    /// SPD-01: an event a commit raises with no paint (an `animate` that
    /// ends at once, to the value the node holds) reaches JS without
    /// waiting for a frame.
    #[test]
    fn commit_events_go_out_without_a_frame() {
        let _serial = crate::GPU_TESTS.lock().unwrap_or_else(|e| e.into_inner());
        if Gpu::try_headless().is_none() {
            eprintln!("no GPU adapter: skipped");
            return;
        }
        let session = Session::new();
        let frames = Arc::new(AtomicU64::new(0));
        let host = session.clone();
        let counter = frames.clone();
        let thread =
            std::thread::spawn(move || run_counted(host, Size::new(100.0, 100.0), 1.0, &counter));
        let submit = |t: &Transaction<'_>| session.submit(craie_ui::wire::encode(t)).unwrap();
        // A liveness bound, not a budget: llvmpipe's first frame took
        // over 5 s on exe1 at load 55.
        let wait = |what: &str, done: &dyn Fn() -> bool| {
            let deadline = Instant::now() + Duration::from_secs(30);
            while !done() {
                assert!(Instant::now() < deadline, "timed out: {what}");
                std::thread::sleep(Duration::from_millis(5));
            }
        };
        let mut t = Transaction::new(1);
        t.create(1, NodeKind::View)
            .fill(1, 0x3040_50FF)
            .place(NIL, 1, NIL);
        submit(&t);
        // The first frame is drawn, then the loop idles (no frame for a
        // while: nothing is owed).
        wait("the first frame", &|| frames.load(Ordering::SeqCst) >= 1);
        let mut settled = frames.load(Ordering::SeqCst);
        loop {
            std::thread::sleep(Duration::from_millis(100));
            let now = frames.load(Ordering::SeqCst);
            if now == settled {
                break;
            }
            settled = now;
        }
        let mut t = Transaction::new(2);
        t.animate(
            1,
            Prop::Opacity,
            Value::Opacity(1.0),
            Timing::curve(0.0, [0.0, 0.0, 1.0, 1.0]),
        );
        submit(&t);
        let ended = std::cell::Cell::new(false);
        wait("the end event", &|| {
            session.pump(|frame| {
                // An events frame: tag 1, a u32 count, records with the
                // kind first.
                if frame.first() == Some(&1)
                    && frame.get(5) == Some(&events::out_kind::ANIMATION_END)
                {
                    ended.set(true);
                }
                Delivery::Sent
            });
            ended.get()
        });
        assert_eq!(
            frames.load(Ordering::SeqCst),
            settled,
            "the event came without a frame"
        );
        session.close("test done");
        thread.join().unwrap();
    }
}
