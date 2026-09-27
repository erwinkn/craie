//! E19 (`EXPERIMENTS.md`), the event round trip: `CRAIE_E19=<csv>`
//! clicks the marker node at `CRAIE_E19_AT` (logical "x,y", default
//! "20,20") `CRAIE_E19_EVENTS` times (default 1000), 30 to 70 ms apart
//! at random. The app answers the n-th click by filling the marker with
//! `n << 8 | 0xff`, so the marker appears as opaque black (no answers);
//! clicks start a second after that. They arrive on their own schedule,
//! not after the previous answer, so a stall is sampled as often as it
//! lasts. The probe stamps each click when it was due (a platform event
//! would have arrived), when it is dispatched (native was free), when
//! the commit that answers it is applied, and when the first frame after
//! that is drawn (headless: the GPU finished; windowed: presented). When
//! every click is drawn, or nothing has happened for 10 s, it writes
//! `seq,due,dispatched,applied,presented` per click (ns on the clock
//! Node's `process.hrtime` reads; 0: never) and closes the session with
//! "e19 done".

use std::path::PathBuf;
use std::time::{Duration, Instant};

use craie_core::rng::Rng;
use craie_ui::bridge::Session;
use craie_ui::events::{Button, Event, Mods};
use craie_ui::host::NodeId;
use craie_ui::ui::Ui;

const GAP_MS: (u32, u32) = (30, 70);
const TIMEOUT: Duration = Duration::from_secs(10);

pub struct Probe {
    out: PathBuf,
    at: (f32, f32),
    want: usize,
    rng: Rng,
    next: Instant,
    target: Option<NodeId>,
    /// Per click: due, dispatched, applied, presented (ns; 0 until then).
    stamps: Vec<[u64; 4]>,
    applied: usize,
    presented: usize,
    progress: Instant,
}

impl Probe {
    pub fn from_env() -> Option<Probe> {
        let out = PathBuf::from(std::env::var_os("CRAIE_E19")?);
        let at = std::env::var("CRAIE_E19_AT")
            .ok()
            .and_then(|v| {
                let (x, y) = v.split_once(',')?;
                Some((x.trim().parse().ok()?, y.trim().parse().ok()?))
            })
            .unwrap_or((20.0, 20.0));
        let want = std::env::var("CRAIE_E19_EVENTS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(1000);
        let now = Instant::now();
        Some(Probe {
            out,
            at,
            want,
            rng: Rng::new(0xE19),
            next: now,
            target: None,
            stamps: Vec::with_capacity(want),
            applied: 0,
            presented: 0,
            progress: now,
        })
    }

    /// When the probe must next run: the next click, or the timeout.
    pub fn due(&self) -> Option<Instant> {
        let click = (self.target.is_some() && self.stamps.len() < self.want).then_some(self.next);
        let timeout = self.progress + TIMEOUT;
        Some(click.map_or(timeout, |c| c.min(timeout)))
    }

    /// The clicks due by now, as events to dispatch at once: those that
    /// fell due while native was busy arrive together, as queued
    /// platform input does.
    pub fn clicks(&mut self) -> Vec<Event> {
        let mut out = Vec::new();
        if self.target.is_none() {
            return out;
        }
        let now = Instant::now();
        let ns = now_ns();
        let (x, y) = self.at;
        let (lo, hi) = GAP_MS;
        while self.stamps.len() < self.want && self.next <= now {
            let due = self.next;
            self.next = due + Duration::from_millis((lo + self.rng.below(hi - lo + 1)) as u64);
            self.stamps
                .push([ns - (now - due).as_nanos() as u64, ns, 0, 0]);
            let button = Button::Primary;
            out.push(Event::PointerDown {
                x,
                y,
                button,
                mods: Mods::default(),
            });
            out.push(Event::PointerUp { x, y, button });
        }
        out
    }

    /// Commits were applied: stamps the clicks the marker's fill now
    /// answers.
    pub fn applied(&mut self, ui: &Ui) {
        let Some(id) = self.target else { return };
        let fill = fill(ui, id);
        let answered = ((fill >> 8) as usize).min(self.stamps.len());
        if fill & 0xff != 0xff || answered <= self.applied {
            return;
        }
        let now = now_ns();
        for s in &mut self.stamps[self.applied..answered] {
            s[2] = now;
        }
        self.applied = answered;
        self.progress = Instant::now();
    }

    /// A frame was drawn: stamps the answers it shows, or finds the
    /// marker (hit testing needs the layout this frame computed).
    pub fn presented(&mut self, ui: &Ui) {
        if self.target.is_none() {
            let (x, y) = self.at;
            if let Some(id) = ui.hit_test(x, y).filter(|&id| fill(ui, id) == 0xff) {
                self.target = Some(id);
                self.progress = Instant::now();
                self.next = self.progress + Duration::from_secs(1);
            }
        }
        let now = now_ns();
        for s in &mut self.stamps[self.presented..self.applied] {
            s[3] = now;
        }
        self.presented = self.applied;
    }

    /// When every click is drawn, or nothing happened for `TIMEOUT` (no
    /// marker, or no answer): writes the results, closes the session,
    /// and returns true.
    pub fn finish(&self, session: &Session) -> bool {
        let over = self.presented == self.want || self.progress.elapsed() > TIMEOUT;
        if over && !session.is_closed() {
            let mut csv = String::from("seq,due,dispatched,applied,presented\n");
            for (i, [u, d, a, p]) in self.stamps.iter().enumerate() {
                csv.push_str(&format!("{},{u},{d},{a},{p}\n", i + 1));
            }
            if let Err(e) = std::fs::write(&self.out, csv) {
                eprintln!("[e19] cannot write {}: {e}", self.out.display());
            }
            session.close("e19 done");
        }
        over
    }
}

fn fill(ui: &Ui, id: NodeId) -> u32 {
    ui.host.paint.get(id.index()).map_or(0, |p| p.fill)
}

/// Nanoseconds on the monotonic clock libuv's `uv_hrtime` reads (Node's
/// `process.hrtime`): CLOCK_MONOTONIC on Linux, mach absolute time
/// (CLOCK_UPTIME_RAW) on macOS, so JS stamps join native ones.
#[cfg(unix)]
pub fn now_ns() -> u64 {
    #[cfg(target_vendor = "apple")]
    const CLOCK: libc::clockid_t = libc::CLOCK_UPTIME_RAW;
    #[cfg(not(target_vendor = "apple"))]
    const CLOCK: libc::clockid_t = libc::CLOCK_MONOTONIC;
    let mut t = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: clock_gettime writes the timespec it is given.
    unsafe { libc::clock_gettime(CLOCK, &mut t) };
    t.tv_sec as u64 * 1_000_000_000 + t.tv_nsec as u64
}

/// Elsewhere a process-local clock: native intervals hold, joins with
/// JS stamps do not.
#[cfg(not(unix))]
pub fn now_ns() -> u64 {
    static START: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    START.get_or_init(Instant::now).elapsed().as_nanos() as u64
}
